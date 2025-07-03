use crate::nintendo::ctr::error::{NintendoCTRError, NintendoCTRResult};
use async_recursion::async_recursion;
use std::path::{Path, PathBuf};
use tokio::fs;

#[async_recursion]
pub async fn get_all_files(dir_path: &Path) -> NintendoCTRResult<Vec<PathBuf>> {
    let mut dir = fs::read_dir(dir_path).await?;
    let mut files = Vec::new();

    while let Some(entry) = dir.next_entry().await? {
        let path = entry.path();

        if path.is_dir() {
            files.append(&mut get_all_files(&path).await?);
        } else {
            files.push(path);
        }
    }

    Ok(files)
}

pub async fn find_title_file(folder_path: &Path) -> NintendoCTRResult<PathBuf> {
    let files = get_all_files(folder_path).await?;

    files
        .iter()
        .find(|file| {
            let file_name = file.file_name().and_then(|n| n.to_str());
            let extension = file.extension().and_then(|s| s.to_str()).unwrap_or("");
            file_name == Some("cetk") || extension == "tik"
        })
        .map(|file| file.to_path_buf())
        .ok_or_else(|| NintendoCTRError::NoTitleFileFound(folder_path.to_path_buf()))
}

pub async fn find_tmd_file(folder_path: &Path) -> NintendoCTRResult<PathBuf> {
    let files = get_all_files(folder_path).await?;

    let mut tmd_files: Vec<_> = files
        .iter()
        .filter_map(|file| {
            let file_name = file.file_name()?.to_str()?;
            let extension = file.extension().and_then(|s| s.to_str()).unwrap_or("");
            let base = file_name
                .strip_suffix(&format!(".{extension}"))
                .unwrap_or(file_name);
            if base == "tmd" { Some(file) } else { None }
        })
        .collect();

    if let Some(tmd_file_exact) = tmd_files.iter().find(|file| {
        file.file_name().and_then(|n| n.to_str()) == Some("tmd") && file.extension().is_none()
    }) {
        return Ok(tmd_file_exact.to_path_buf());
    }

    tmd_files.sort_by_key(|file| {
        file.extension()
            .and_then(|s| s.to_str()?.parse::<u32>().ok())
            .unwrap_or(0)
    });

    tmd_files
        .first()
        .map(|file| file.to_path_buf())
        .ok_or_else(|| NintendoCTRError::NoTmdFileFound(folder_path.to_path_buf()))
}
