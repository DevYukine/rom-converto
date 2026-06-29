use crate::info_cache::InfoCache;
use crate::progress::TauriProgress;
use rom_converto_lib::chd::{
    ChdDvdOptions, DiscMode, convert_disc_to_chd_cancellable, extract_from_chd_cancellable,
    verify_chd_cancellable,
};
use rom_converto_lib::cso::{
    CsoCompressOptions, CsoFormat, compress_to_cso_cancellable, decompress_from_cso_cancellable,
    verify_cso,
};
use rom_converto_lib::cue::merge::merge_bin;
use rom_converto_lib::info::{InfoOptions, InfoResult, read_info};
use rom_converto_lib::nintendo::ctr::convert::{convert_rom_cancellable, derive_converted_path};
use rom_converto_lib::nintendo::ctr::verify::{CtrVerifyOptions, verify_ctr};
use rom_converto_lib::nintendo::ctr::z3ds::{
    compress_rom_cancellable, decompress_rom_cancellable, derive_compressed_path,
    derive_decompressed_path,
};
use rom_converto_lib::nintendo::ctr::{
    CdnToCiaOptions, convert_cdn_to_cia_cancellable, decrypt_rom_cancellable,
    generate_ticket_from_cdn,
};
use rom_converto_lib::nintendo::dol::verify::{DolVerifyOptions, verify_dol};
use rom_converto_lib::nintendo::nx::{
    NczMode, NxCompressOptions, compress_container_async_cancellable,
    decompress_container_async_cancellable, derive_compressed_path as nx_derive_compressed_path,
    derive_decompressed_path as nx_derive_decompressed_path, detect_container, load_keyset,
    verify_container_async,
};
use rom_converto_lib::nintendo::rvl::verify::{RvlVerifyOptions, verify_rvl};
use rom_converto_lib::nintendo::rvz::{
    RvzCompressOptions, compress_disc_cancellable, decompress_disc_cancellable,
    decompress_disc_to_wbfs_cancellable, derive_disc_path, derive_rvz_path,
};
use rom_converto_lib::nintendo::wup::{
    TitleInput, WupCompressOptions, compress_titles_async_cancellable,
    decrypt_nus_title_async_cancellable, verify_wup_async,
};
use rom_converto_lib::util::CancelToken;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, State};

fn err_to_string(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Single-slot holder for the token of the operation currently running.
/// Only one conversion runs at a time per command invocation, so a
/// single slot is enough; `cmd_cancel` fires whatever is in it.
pub type ActiveCancel = Arc<tokio::sync::Mutex<Option<CancelToken>>>;

async fn begin(state: &State<'_, ActiveCancel>) -> CancelToken {
    let token = CancelToken::new();
    *state.lock().await = Some(token.clone());
    token
}

async fn finish(state: &State<'_, ActiveCancel>) {
    *state.lock().await = None;
}

#[tauri::command]
pub async fn cmd_cancel(state: State<'_, ActiveCancel>) -> Result<(), String> {
    if let Some(token) = state.lock().await.as_ref() {
        token.cancel();
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cmd_cdn_to_cia(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
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
        output_dir: None,
        on_conflict: rom_converto_lib::util::ConflictPolicy::Overwrite,
    };
    let token = begin(&state).await;
    // The streaming decrypt holds the worker-pool receiver across await points,
    // so its future is not Send; run on a dedicated thread with its own runtime.
    let result = std::thread::spawn(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(err_to_string)?;
        rt.block_on(convert_cdn_to_cia_cancellable(
            opts,
            progress.as_ref(),
            total_progress.as_ref(),
            token,
        ))
        .map_err(err_to_string)
    })
    .join()
    .map_err(|_| "task panicked".to_string());
    finish(&state).await;
    result??;
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
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: PathBuf,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "decrypt"));
    let out_display = output.display().to_string();
    let token = begin(&state).await;
    // The streaming decrypt holds the worker-pool receiver across await points,
    // so its future is not Send; run on a dedicated thread with its own runtime.
    let result = std::thread::spawn(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(err_to_string)?;
        rt.block_on(decrypt_rom_cancellable(
            &input,
            &output,
            progress.as_ref(),
            token,
        ))
        .map_err(err_to_string)
    })
    .join()
    .map_err(|_| "task panicked".to_string());
    finish(&state).await;
    result??;
    Ok(format!("Decrypted to {out_display}"))
}

#[tauri::command]
pub async fn cmd_compress_rom(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: Option<PathBuf>,
    level: Option<i32>,
    allow_encrypted: bool,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "compress"));
    let output = output.unwrap_or_else(|| derive_compressed_path(&input));
    let out_display = output.display().to_string();
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        compress_rom_cancellable(
            &input,
            &output,
            level,
            allow_encrypted,
            progress.as_ref(),
            token,
        )
        .await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    Ok(format!("Compressed to {out_display}"))
}

#[tauri::command]
pub async fn cmd_decompress_rom(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: Option<PathBuf>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "decompress"));
    let output = output.unwrap_or_else(|| derive_decompressed_path(&input));
    let out_display = output.display().to_string();
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        decompress_rom_cancellable(&input, &output, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    Ok(format!("Decompressed to {out_display}"))
}

#[tauri::command]
pub async fn cmd_chd_compress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input_path: PathBuf,
    output: PathBuf,
    force: bool,
    zstd: Option<bool>,
    hunk_size: Option<u32>,
    mode: Option<String>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "chd-compress"));
    let out_display = output.display().to_string();
    let mode = match mode.as_deref() {
        Some("cd") => Some(DiscMode::Cd),
        Some("dvd") => Some(DiscMode::Dvd),
        _ => None,
    };
    let opts = ChdDvdOptions {
        hunk_size,
        allow_zstd: zstd.unwrap_or(false),
        force,
    };
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        convert_disc_to_chd_cancellable(progress.as_ref(), input_path, output, mode, opts, token)
            .await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    Ok(format!("CHD created at {out_display}"))
}

#[tauri::command]
pub async fn cmd_cso_compress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input_path: PathBuf,
    output: PathBuf,
    format: String,
    force: bool,
    block_size: Option<u32>,
) -> Result<String, String> {
    let format = match format.as_str() {
        "zso" => CsoFormat::Zso,
        _ => CsoFormat::Cso,
    };
    let progress = Arc::new(TauriProgress::new(app, "cso-compress"));
    let out_display = output.display().to_string();
    let opts = CsoCompressOptions {
        format,
        block_size,
        force,
    };
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        compress_to_cso_cancellable(progress.as_ref(), input_path, output, opts, token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    Ok(format!("{} created at {out_display}", format.name()))
}

#[tauri::command]
pub async fn cmd_cso_decompress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input_path: PathBuf,
    output: PathBuf,
    force: bool,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "cso-decompress"));
    let out_display = output.display().to_string();
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        decompress_from_cso_cancellable(progress.as_ref(), input_path, output, force, token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    Ok(format!("ISO restored at {out_display}"))
}

#[tauri::command]
pub async fn cmd_cso_verify(
    app: AppHandle,
    input_path: PathBuf,
    full: bool,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "cso-verify"));
    tokio::spawn(async move { verify_cso(progress.as_ref(), input_path.clone(), full).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;
    Ok(if full {
        "Index structure OK, all blocks decoded successfully".to_string()
    } else {
        "Index structure OK".to_string()
    })
}

#[tauri::command]
pub async fn cmd_cue_merge(
    app: AppHandle,
    cue_path: PathBuf,
    output: PathBuf,
    force: bool,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "cue-merge"));
    let out_display = output.display().to_string();
    tokio::spawn(async move { merge_bin(progress.as_ref(), cue_path, output, force).await })
        .await
        .map_err(err_to_string)?
        .map_err(err_to_string)?;
    Ok(format!("Merged bin/cue created at {out_display}"))
}

// CHD extract and verify use deeply nested async types from ChdReader
// that exceed the compiler's recursion limit for Send inference. We run
// these on a dedicated thread with its own tokio runtime to sidestep the issue.

#[tauri::command]
pub async fn cmd_chd_extract(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: PathBuf,
    parent: Option<PathBuf>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "chd-extract"));
    let out_display = output.display().to_string();
    let token = begin(&state).await;
    // ChdReader's deeply nested async types exceed the compiler's Send recursion
    // limit, so we run on a dedicated thread with its own tokio runtime.
    let result = std::thread::spawn(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(err_to_string)?;
        rt.block_on(extract_from_chd_cancellable(
            progress.as_ref(),
            input,
            output,
            parent,
            token,
        ))
        .map_err(err_to_string)
    })
    .join()
    .map_err(|_| "task panicked".to_string());
    finish(&state).await;
    result??;
    Ok(format!("Extracted to {out_display}"))
}

#[tauri::command]
pub async fn cmd_chd_verify(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    parent: Option<PathBuf>,
    fix: bool,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "chd-verify"));
    let token = begin(&state).await;
    let result = std::thread::spawn(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(err_to_string)?;
        rt.block_on(verify_chd_cancellable(
            progress.as_ref(),
            input,
            parent,
            fix,
            token,
        ))
        .map_err(err_to_string)
    })
    .join()
    .map_err(|_| "task panicked".to_string());
    finish(&state).await;
    result??;
    Ok("CHD verification passed".to_string())
}

#[tauri::command]
pub async fn cmd_compress_disc(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: Option<PathBuf>,
    level: Option<i32>,
    chunk_size: Option<u32>,
    task_id: String,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, &task_id));
    let output = output.unwrap_or_else(|| derive_rvz_path(&input));
    let out_display = output.display().to_string();
    let opts = RvzCompressOptions {
        compression_level: level.unwrap_or(RvzCompressOptions::default().compression_level),
        chunk_size: chunk_size.unwrap_or(RvzCompressOptions::default().chunk_size),
        ..RvzCompressOptions::default()
    };
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        compress_disc_cancellable(&input, &output, opts, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    Ok(format!("Compressed to {out_display}"))
}

#[tauri::command]
pub async fn cmd_decompress_disc(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: Option<PathBuf>,
    task_id: String,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, &task_id));
    let output = output.unwrap_or_else(|| derive_disc_path(&input));
    let out_display = output.display().to_string();
    let to_wbfs = output
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.eq_ignore_ascii_case("wbfs"))
        .unwrap_or(false);
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        if to_wbfs {
            decompress_disc_to_wbfs_cancellable(&input, &output, progress.as_ref(), token).await
        } else {
            decompress_disc_cancellable(&input, &output, progress.as_ref(), token).await
        }
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    Ok(format!("Decompressed to {out_display}"))
}

#[tauri::command]
pub async fn cmd_wup_compress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
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
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        compress_titles_async_cancellable(titles, output, opts, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    Ok(format!("Compressed to {out_display}"))
}

#[tauri::command]
pub async fn cmd_wup_decrypt(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: PathBuf,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "wup-decrypt"));
    let out_display = output.display().to_string();
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        decrypt_nus_title_async_cancellable(input, output, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    Ok(format!("Decrypted to {out_display}"))
}

#[tauri::command]
pub async fn cmd_nx_compress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
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
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        compress_container_async_cancellable(input, output, opts, keys, progress.as_ref(), token)
            .await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
    Ok(format!("Compressed to {out_display}"))
}

#[tauri::command]
pub async fn cmd_nx_decompress(
    app: AppHandle,
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: Option<PathBuf>,
    keys: Option<PathBuf>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "nx-decompress"));
    let output = output.unwrap_or_else(|| nx_derive_decompressed_path(&input));
    let out_display = output.display().to_string();
    let keys = load_keyset(keys.as_deref()).map_err(err_to_string)?;
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        decompress_container_async_cancellable(input, output, keys, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
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
    state: State<'_, ActiveCancel>,
    input: PathBuf,
    output: Option<PathBuf>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "ctr-convert"));
    let output = output.unwrap_or_else(|| derive_converted_path(&input));
    let out_display = output.display().to_string();
    let token = begin(&state).await;
    let result = tokio::spawn(async move {
        convert_rom_cancellable(&input, &output, progress.as_ref(), token).await
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string);
    finish(&state).await;
    result?;
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
pub async fn cmd_verify_dol(app: AppHandle, input: PathBuf, full: bool) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "dol-verify"));
    let result = tokio::task::spawn_blocking(move || {
        let opts = DolVerifyOptions { full };
        verify_dol(&input, &opts, progress.as_ref())
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;

    serde_json::to_string(&result).map_err(err_to_string)
}

#[tauri::command]
pub async fn cmd_verify_rvl(app: AppHandle, input: PathBuf, full: bool) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "rvl-verify"));
    let result = tokio::task::spawn_blocking(move || {
        let opts = RvlVerifyOptions { full };
        verify_rvl(&input, &opts, progress.as_ref())
    })
    .await
    .map_err(err_to_string)?
    .map_err(err_to_string)?;

    serde_json::to_string(&result).map_err(err_to_string)
}

#[tauri::command]
pub async fn cmd_wup_verify(
    app: AppHandle,
    input: PathBuf,
    keys: Option<PathBuf>,
) -> Result<String, String> {
    let progress = Arc::new(TauriProgress::new(app, "wup-verify"));
    let result =
        tokio::spawn(async move { verify_wup_async(input, keys, progress.as_ref()).await })
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

#[tauri::command]
pub fn app_display_version() -> &'static str {
    env!("ROM_CONVERTO_DISPLAY_VERSION")
}

fn extract_icon_png(info: &InfoResult) -> Option<Vec<u8>> {
    match info {
        InfoResult::Ctr(c) => c.icon.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Dol(d) => d.banner_image.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Rvl(r) => r.image.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Wup(w) => w.image.as_ref().map(|i| i.png_bytes.clone()),
        InfoResult::Nx(n) => n
            .full
            .as_ref()
            .and_then(|f| f.control.as_ref())
            .and_then(|c| c.icon.as_ref())
            .map(|i| i.png_bytes.clone()),
        InfoResult::Chd(_) => None,
        InfoResult::Cso(_) => None,
    }
}
