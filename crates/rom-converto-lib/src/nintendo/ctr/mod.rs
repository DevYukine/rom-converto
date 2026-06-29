use crate::nintendo::ctr::cia::{decrypt_from_encrypted_cia, write_cia};
use crate::nintendo::ctr::constants::NCCH_MAGIC_OFFSET;
use crate::nintendo::ctr::decrypt::cia::{parse_and_decrypt_ncch, parse_and_decrypt_ncsd};
use crate::nintendo::ctr::error::NintendoCTRError;
use crate::nintendo::ctr::models::cia::CIA_HEADER_SIZE;
use crate::nintendo::ctr::models::ticket::Ticket;
use crate::nintendo::ctr::models::title_metadata::TitleMetadata;
use crate::nintendo::ctr::title_key::generate_title_key;
use crate::nintendo::ctr::util::fs::{find_title_file, find_tmd_file};
use crate::nintendo::ctr::z3ds::models::underlying_magic;
use crate::nintendo::ctr::z3ds::{
    compress_rom_cancellable, derive_compressed_path, derive_decompressed_path,
};
use crate::util::{
    CancelToken, ConflictPolicy, ConflictResolution, ProgressReporter, resolve_conflict,
    scratch_output_path,
};
use anyhow::Result;
use binrw::BinRead;
use futures::TryFutureExt;
use log::{debug, info, warn};
use std::io::{Cursor, SeekFrom};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter};

mod cia;
mod constants;
pub mod convert;
mod decrypt;
pub mod error;
pub mod exefs;
pub mod info;
pub mod models;
pub mod seed;
#[cfg(test)]
mod test_fixtures;
pub mod title_key;
mod util;
pub mod verify;
pub mod z3ds;

#[derive(Debug, Clone)]
pub struct CdnToCiaOptions {
    pub cdn_dir: PathBuf,
    pub output: Option<PathBuf>,
    pub cleanup: bool,
    pub recursive: bool,
    pub ensure_ticket_exists: bool,
    pub decrypt: bool,
    pub compress: bool,
    pub output_dir: Option<PathBuf>,
    pub on_conflict: ConflictPolicy,
}

pub fn derive_decrypted_path(input: &Path) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    let ext = input.extension().and_then(|s| s.to_str()).unwrap_or("");
    let name = if ext.is_empty() {
        format!("{stem}.decrypted")
    } else {
        format!("{stem}.decrypted.{ext}")
    };
    input.with_file_name(name)
}

const DECRYPT_EXTS: &[&str] = &["cia", "3ds", "cci", "cxi"];

pub async fn decrypt_cia(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
) -> Result<()> {
    decrypt_cia_cancellable(input, output, progress, CancelToken::new()).await
}

pub async fn decrypt_cia_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<()> {
    let tmp = scratch_output_path(output);
    let out = File::create(&tmp).await?;
    let mut out = BufWriter::new(out);

    if let Err(err) = decrypt_from_encrypted_cia(input, &mut out, progress, &cancel).await {
        drop(out);
        fs::remove_file(&tmp).await.ok();
        return Err(err);
    }

    out.flush().await?;
    drop(out);
    fs::rename(&tmp, output).await?;

    info!("Successfully decrypted CIA file");

    Ok(())
}

pub async fn decrypt_rom(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
) -> Result<()> {
    decrypt_rom_cancellable(input, output, progress, CancelToken::new()).await
}

pub async fn decrypt_rom_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<()> {
    let file_size = tokio::fs::metadata(input).await?.len();
    progress.start(file_size, "Decrypting...");

    let mut file = File::open(input).await?;

    // Read magic at offset 0x100 (shared by NCSD and NCCH)
    let mut magic_buf = [0u8; 4];
    file.seek(SeekFrom::Start(NCCH_MAGIC_OFFSET as u64)).await?;
    file.read_exact(&mut magic_buf).await?;
    drop(file);

    if magic_buf == underlying_magic::NCSD {
        info!("Detected NCSD format (.3ds/.cci)");
        decrypt_ncsd_cancellable(input, output, progress, &cancel).await?;
    } else if magic_buf == underlying_magic::NCCH {
        info!("Detected standalone NCCH format (.cxi)");
        decrypt_ncch_cancellable(input, output, progress, &cancel).await?;
    } else {
        // Try CIA: check if the u32 at offset 0 matches CIA_HEADER_SIZE
        let mut file = File::open(input).await?;
        let mut header_check = [0u8; 4];
        file.read_exact(&mut header_check).await?;
        drop(file);

        let header_size = u32::from_le_bytes(header_check);
        if header_size == CIA_HEADER_SIZE {
            info!("Detected CIA format");
            decrypt_cia_cancellable(input, output, progress, cancel).await?;
        } else {
            return Err(anyhow::anyhow!(
                "Unrecognized format: no NCSD/NCCH magic at 0x100 and not a CIA file"
            ));
        }
    }

    progress.finish();

    Ok(())
}

async fn decrypt_ncsd_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<()> {
    let tmp = scratch_output_path(output);

    // The verbatim copy carries the NCSD header, inter-partition gaps, and any
    // plain partitions; parse_and_decrypt_ncsd overwrites each NCCH partition
    // region in place, so the decrypt streams straight into the final temp
    // without per-partition scratch files.
    let result = async {
        fs::copy(input, &tmp).await?;
        let mut out = fs::OpenOptions::new().write(true).read(true).open(&tmp).await?;
        parse_and_decrypt_ncsd(input, &mut out, None, progress, cancel).await?;
        out.flush().await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Err(err) = result {
        fs::remove_file(&tmp).await.ok();
        return Err(err);
    }

    fs::rename(&tmp, output).await?;

    info!("Successfully decrypted NCSD file");
    Ok(())
}

async fn decrypt_ncch_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<()> {
    let tmp = scratch_output_path(output);

    let result = async {
        let mut out = File::create(&tmp).await?;
        parse_and_decrypt_ncch(input, &mut out, progress, cancel).await?;
        out.flush().await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Err(err) = result {
        fs::remove_file(&tmp).await.ok();
        return Err(err);
    }

    fs::rename(&tmp, output).await?;

    info!("Successfully decrypted NCCH file");
    Ok(())
}

pub async fn generate_ticket_from_cdn(cdn_dir: &Path, output: &Path) -> Result<()> {
    let tmd_path = find_tmd_file(cdn_dir).await?;
    debug!("Found TMD file at {}", tmd_path.display());

    let mut ticket_metadata_data = Cursor::new(fs::read(&tmd_path).await?);
    let title_metadata = TitleMetadata::read(&mut ticket_metadata_data)?;

    let title_id_str = format!("{:016X}", title_metadata.header.title_id);

    let title_key = generate_title_key(&title_id_str, None)?;

    const CETK_STRING_TEMPLATE: &str = "00010004d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000526f6f742d434130303030303030332d585330303030303030630000000000000000000000000000000000000000000000000000000000000000000000000000feedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedface010000eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee00000000000000000000000000dddddddddddddddd00001111000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010014000000ac000000140001001400000000000000280000000100000084000000840003000000000000ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010004919ebe464ad0f552cd1b72e7884910cf55a9f02e50789641d896683dc005bd0aea87079d8ac284c675065f74c8bf37c88044409502a022980bb8ad48383f6d28a79de39626ccb2b22a0f19e41032f094b39ff0133146dec8f6c1a9d55cd28d9e1c47b3d11f4f5426c2c780135a2775d3ca679bc7e834f0e0fb58e68860a71330fc95791793c8fba935a7a6908f229dee2a0ca6b9b23b12d495a6fe19d0d72648216878605a66538dbf376899905d3445fc5c727a0e13e0e2c8971c9cfa6c60678875732a4e75523d2f562f12aabd1573bf06c94054aefa81a71417af9a4a066d0ffc5ad64bab28b1ff60661f4437d49e1e0d9412eb4bcacf4cfd6a3408847982000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000526f6f742d43413030303030303033000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000158533030303030303063000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000137a0894ad505bb6c67e2e5bdd6a3bec43d910c772e9cc290da58588b77dcc11680bb3e29f4eabbb26e98c2601985c041bb14378e689181aad770568e928a2b98167ee3e10d072beef1fa22fa2aa3e13f11e1836a92a4281ef70aaf4e462998221c6fbb9bdd017e6ac590494e9cea9859ceb2d2a4c1766f2c33912c58f14a803e36fccdcccdc13fd7ae77c7a78d997e6acc35557e0d3e9eb64b43c92f4c50d67a602deb391b06661cd32880bd64912af1cbcb7162a06f02565d3b0ece4fcecddae8a4934db8ee67f3017986221155d131c6c3f09ab1945c206ac70c942b36f49a1183bcd78b6e4b47c6c5cac0f8d62f897c6953dd12f28b70c5b7df751819a9834652625000100010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010003704138efbbbda16a987dd901326d1c9459484c88a2861b91a312587ae70ef6237ec50e1032dc39dde89a96a8e859d76a98a6e7e36a0cfe352ca893058234ff833fcb3b03811e9f0dc0d9a52f8045b4b2f9411b67a51c44b5ef8ce77bd6d56ba75734a1856de6d4bed6d3a242c7c8791b3422375e5c779abf072f7695efa0f75bcb83789fc30e3fe4cc8392207840638949c7f688565f649b74d63d8d58ffadda571e9554426b1318fc468983d4c8a5628b06b6fc5d507c13e7a18ac1511eb6d62ea5448f83501447a9afb3ecc2903c9dd52f922ac9acdbef58c6021848d96e208732d3d1d9d9ea440d91621c7a99db8843c59c1f2e2c7d9b577d512c166d6f7e1aad4a774a37447e78fe2021e14a95d112a068ada019f463c7a55685aabb6888b9246483d18b9c806f474918331782344a4b8531334b26303263d9d2eb4f4bb99602b352f6ae4046c69a5e7e8e4a18ef9bc0a2ded61310417012fd824cc116cfb7c4c1f7ec7177a17446cbde96f3edd88fcd052f0b888a45fdaf2b631354f40d16e5fa9c2c4eda98e798d15e6046dc5363f3096b2c607a9d8dd55b1502a6ac7d3cc8d8c575998e7d796910c804c495235057e91ecd2637c9c1845151ac6b9a0490ae3ec6f47740a0db0ba36d075956cee7354ea3e9a4f2720b26550c7d394324bc0cb7e9317d8a8661f42191ff10b08256ce3fd25b745e5194906b4d61cb4c2e000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000526f6f7400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001434130303030303030330000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000007be8ef6cb279c9e2eee121c6eaf44ff639f88f078b4b77ed9f9560b0358281b50e55ab721115a177703c7a30fe3ae9ef1c60bc1d974676b23a68cc04b198525bc968f11de2db50e4d9e7f071e562dae2092233e9d363f61dd7c19ff3a4a91e8f6553d471dd7b84b9f1b8ce7335f0f5540563a1eab83963e09be901011f99546361287020e9cc0dab487f140d6626a1836d27111f2068de4772149151cf69c61ba60ef9d949a0f71f5499f2d39ad28c7005348293c431ffbd33f6bca60dc7195ea2bcc56d200baf6d06d09c41db8de9c720154ca4832b69c08c69cd3b073a0063602f462d338061a5ea6c915cd5623579c3eb64ce44ef586d14baaa8834019b3eebeed3790001000100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

    let title_version_hex = format!("{:04x}", title_metadata.header.title_version);

    let cetk = CETK_STRING_TEMPLATE
        .replace("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", &title_key)
        .replace("1111", &title_version_hex)
        .replace("dddddddddddddddd", &title_id_str);

    let mut file = File::create(&output).await?;

    file.write_all(&hex::decode(cetk)?).await?;

    info!("✅ Successfully created Ticket at {}", output.display());

    Ok(())
}

pub async fn convert_cdn_to_cia(
    opts: CdnToCiaOptions,
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
) -> Result<()> {
    convert_cdn_to_cia_cancellable(opts, progress, total_progress, CancelToken::new()).await
}

pub async fn convert_cdn_to_cia_cancellable(
    opts: CdnToCiaOptions,
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<()> {
    if opts.recursive {
        let mut count: u64 = 0;
        let mut dirs = tokio::fs::read_dir(&opts.cdn_dir).await?;
        while let Ok(Some(entry)) = dirs.next_entry().await {
            if entry.path().is_dir() {
                count += 1;
            }
        }

        total_progress.start(count, &format!("Processing {count} directories..."));

        if let Some(dir) = opts.output_dir.as_deref() {
            fs::create_dir_all(dir).await?;
        }

        let mut directories = tokio::fs::read_dir(&opts.cdn_dir).await?;

        while let Ok(Some(entry)) = directories.next_entry().await {
            if cancel.is_cancelled() {
                total_progress.finish();
                return Err(NintendoCTRError::Cancelled.into());
            }

            debug!("Processing directory: {}", entry.path().display());

            if entry.path().is_file() {
                continue;
            }

            let child_dir = entry.path();
            let mut opts_clone = opts.clone();
            opts_clone.output = opts.output_dir.as_deref().and_then(|dir| {
                child_dir.file_name().map(|name| {
                    let derived =
                        child_dir.with_file_name(format!("{}.cia", name.to_string_lossy()));
                    crate::util::place_in_dir(&derived, Some(dir))
                })
            });
            opts_clone.cdn_dir = child_dir;

            if let Err(err) =
                convert_cdn_to_cia_single(opts_clone, progress, cancel.clone()).await
            {
                warn!(
                    "Failed to convert CDN directory {}: {}",
                    entry.path().display(),
                    err
                );
            }

            total_progress.inc(1);
        }

        total_progress.finish();
        Ok(())
    } else {
        convert_cdn_to_cia_single(opts, progress, cancel).await
    }
}

async fn convert_cdn_to_cia_single(
    opts: CdnToCiaOptions,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<()> {
    let output = match opts.output {
        Some(path) => path,
        None => {
            let name = opts
                .cdn_dir
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| format!("{name}.cia"))
                .ok_or_else(|| anyhow::anyhow!("CDN directory path has no name"))?;

            let parent = opts.cdn_dir.parent().unwrap_or_else(|| Path::new("."));
            parent.join(name)
        }
    };

    // Conflict resolution runs against the final artifact. With --compress that
    // is the .zcia, while the intermediate .cia keeps the working stem, so a
    // rename slot is mapped back through derive_decompressed_path.
    let final_path = if opts.compress {
        derive_compressed_path(&output)
    } else {
        output.clone()
    };
    let output = match resolve_conflict(&final_path, opts.on_conflict)? {
        ConflictResolution::Skip => {
            info!("Skipping, output already exists: {}", final_path.display());
            return Ok(());
        }
        ConflictResolution::Write(resolved) => {
            if opts.compress {
                derive_decompressed_path(&resolved)
            } else {
                resolved
            }
        }
    };

    let cdn_dir = &opts.cdn_dir;

    let ticket_path = find_title_file(cdn_dir)
        .or_else(|err| async {
            if opts.ensure_ticket_exists {
                let path = cdn_dir.join("ticket.tik");
                debug!("Path for ticket file: {}", path.display());
                debug!("CDN Directory: {}", cdn_dir.display());
                generate_ticket_from_cdn(cdn_dir, &path).await?;
                Ok::<PathBuf, anyhow::Error>(path)
            } else {
                Err(err.into())
            }
        })
        .await?;
    debug!("Found Ticket file at {}", ticket_path.display());

    let title_metadata_path = find_tmd_file(cdn_dir).await?;
    debug!("Found TMD file at {}", title_metadata_path.display());

    let mut ticket_metadata_data = Cursor::new(fs::read(&title_metadata_path).await?);
    let title_metadata = TitleMetadata::read(&mut ticket_metadata_data)?;

    let mut ticket_data = Cursor::new(fs::read(&ticket_path).await?);
    let ticket = Ticket::read(&mut ticket_data)?;

    debug!("Processing CIA conversion");

    let ticket_title_id = ticket.ticket_data.title_id;
    let title_metadata_title_id = title_metadata.header.title_id;

    if ticket_title_id != title_metadata_title_id {
        warn!(
            "warning: TICKET and TMD Title IDs do not match: TICKET=0x{ticket_title_id:016X}, TMD=0x{title_metadata_title_id:016X}"
        );
    }

    let tmp = scratch_output_path(&output);
    let out = File::create(&tmp).await?;
    let mut out_buffered = BufWriter::new(out);
    if let Err(err) = write_cia(
        cdn_dir,
        &mut out_buffered,
        &title_metadata_path,
        &ticket_path,
        title_metadata,
        ticket,
        progress,
        &cancel,
    )
    .await
    {
        drop(out_buffered);
        fs::remove_file(&tmp).await.ok();
        return Err(err);
    }
    out_buffered.flush().await?;
    drop(out_buffered);
    fs::rename(&tmp, &output).await?;

    info!("Successfully created CIA file {}", output.display());

    if opts.decrypt {
        let decrypted_cia_path = output.with_extension("decrypted.cia");

        if let Err(err) =
            decrypt_cia_cancellable(&output, &decrypted_cia_path, progress, cancel.clone()).await
        {
            fs::remove_file(&decrypted_cia_path).await.ok();
            fs::remove_file(&output).await.ok();
            return Err(err);
        }

        fs::remove_file(&output).await?;
        fs::rename(&decrypted_cia_path, &output).await?;

        debug!("Deleted original encrypted CIA file: {}", output.display());
    }

    if opts.compress {
        let compressed_path = derive_compressed_path(&output);

        if let Err(err) =
            compress_rom_cancellable(&output, &compressed_path, None, false, progress, cancel).await
        {
            fs::remove_file(&output).await.ok();
            return Err(err.into());
        }

        fs::remove_file(&output).await?;

        debug!("Deleted intermediate CIA file: {}", output.display());
    }

    if opts.cleanup {
        fs::remove_dir_all(cdn_dir).await?;

        debug!("Deleted CDN directory: {}", cdn_dir.display());
    }

    Ok(())
}

pub async fn decrypt_rom_batch(
    input_dir: &Path,
    output_dir: Option<&Path>,
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    max_depth: Option<usize>,
) -> Result<()> {
    decrypt_rom_batch_cancellable(
        input_dir,
        output_dir,
        progress,
        total_progress,
        max_depth,
        CancelToken::new(),
    )
    .await
}

pub async fn decrypt_rom_batch_cancellable(
    input_dir: &Path,
    output_dir: Option<&Path>,
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    max_depth: Option<usize>,
    cancel: CancelToken,
) -> Result<()> {
    let roms = crate::util::fs::collect_files_with_exts(input_dir, DECRYPT_EXTS, max_depth)?;
    if roms.is_empty() {
        warn!(
            "No supported ROM files found in {} (looked for {:?})",
            input_dir.display(),
            DECRYPT_EXTS
        );
        return Ok(());
    }

    total_progress.start(roms.len() as u64, &format!("Decrypting {} files...", roms.len()));

    if let Some(dir) = output_dir {
        fs::create_dir_all(dir).await?;
    }

    for path in roms {
        if cancel.is_cancelled() {
            return Err(NintendoCTRError::Cancelled.into());
        }
        let output =
            crate::util::place_in_dir_mirrored(&derive_decrypted_path(&path), input_dir, output_dir);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).await?;
        }
        debug!("Decrypting {} -> {}", path.display(), output.display());

        if let Err(err) = decrypt_rom_cancellable(&path, &output, progress, cancel.clone()).await {
            if matches!(
                err.downcast_ref::<NintendoCTRError>(),
                Some(NintendoCTRError::Cancelled)
            ) {
                return Err(err);
            }
            warn!("Failed to decrypt {}: {err}", path.display());
        }

        total_progress.inc(1);
    }

    total_progress.finish();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::models::cia::CiaFile;
    use crate::nintendo::ctr::test_fixtures::{append_be, make_cert, make_ticket, make_tmd};
    use crate::util::NoProgress;
    use binrw::Endian;
    use sha2::{Digest, Sha256};

    fn write_cdn_title(dir: &Path, title_id: u64) {
        std::fs::create_dir_all(dir).unwrap();

        let content: Vec<u8> = (0..0x400u32).map(|i| i as u8).collect();
        let mut hasher = Sha256::new();
        hasher.update(&content);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&hasher.finalize());

        std::fs::write(dir.join("00000000"), &content).unwrap();

        let tmd = make_tmd(title_id, vec![(0, 0, content.clone(), hash)]);
        let mut tmd_buf = Vec::new();
        append_be(&mut tmd_buf, &tmd);
        append_be(&mut tmd_buf, &make_cert(b"CP0000000b", 0xBB));
        append_be(&mut tmd_buf, &make_cert(b"CA00000003", 0xAA));
        std::fs::write(dir.join("tmd"), &tmd_buf).unwrap();

        let ticket = make_ticket(title_id);
        let mut tik_buf = Vec::new();
        append_be(&mut tik_buf, &ticket);
        append_be(&mut tik_buf, &make_cert(b"XS0000000c", 0xCC));
        std::fs::write(dir.join("cetk"), &tik_buf).unwrap();
    }

    fn recursive_opts(root: PathBuf, on_conflict: ConflictPolicy) -> CdnToCiaOptions {
        CdnToCiaOptions {
            cdn_dir: root,
            output: None,
            cleanup: false,
            recursive: true,
            ensure_ticket_exists: false,
            decrypt: false,
            compress: false,
            output_dir: None,
            on_conflict,
        }
    }

    fn parses_as_cia(path: &Path) -> bool {
        let bytes = std::fs::read(path).unwrap();
        CiaFile::read_options(&mut Cursor::new(&bytes), Endian::Little, ()).is_ok()
    }

    #[tokio::test]
    async fn cdn_to_cia_recursive_converts_each_subfolder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let base = 0x0004000000030000u64;
        for (i, name) in ["title_a", "title_b", "title_c"].iter().enumerate() {
            write_cdn_title(&root.join(name), base + i as u64);
        }

        let opts = recursive_opts(root.to_path_buf(), ConflictPolicy::Error);
        convert_cdn_to_cia(opts, &NoProgress, &NoProgress)
            .await
            .unwrap();

        for name in ["title_a.cia", "title_b.cia", "title_c.cia"] {
            let out = root.join(name);
            assert!(out.exists(), "missing {}", out.display());
            assert!(parses_as_cia(&out), "{} is not a valid CIA", out.display());
        }
    }

    #[tokio::test]
    async fn cdn_to_cia_recursive_default_error_does_not_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_cdn_title(&root.join("title_a"), 0x0004000000030000);
        let existing = root.join("title_a.cia");
        std::fs::write(&existing, b"PREEXISTING").unwrap();

        let opts = recursive_opts(root.to_path_buf(), ConflictPolicy::Error);
        convert_cdn_to_cia(opts, &NoProgress, &NoProgress)
            .await
            .unwrap();

        assert_eq!(std::fs::read(&existing).unwrap(), b"PREEXISTING");
    }

    #[tokio::test]
    async fn cdn_to_cia_recursive_skip_keeps_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_cdn_title(&root.join("title_a"), 0x0004000000030000);
        let existing = root.join("title_a.cia");
        std::fs::write(&existing, b"PREEXISTING").unwrap();

        let opts = recursive_opts(root.to_path_buf(), ConflictPolicy::Skip);
        convert_cdn_to_cia(opts, &NoProgress, &NoProgress)
            .await
            .unwrap();

        assert_eq!(std::fs::read(&existing).unwrap(), b"PREEXISTING");
    }

    #[tokio::test]
    async fn cdn_to_cia_recursive_overwrite_replaces() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_cdn_title(&root.join("title_a"), 0x0004000000030000);
        let existing = root.join("title_a.cia");
        std::fs::write(&existing, b"PREEXISTING").unwrap();

        let opts = recursive_opts(root.to_path_buf(), ConflictPolicy::Overwrite);
        convert_cdn_to_cia(opts, &NoProgress, &NoProgress)
            .await
            .unwrap();

        assert_ne!(std::fs::read(&existing).unwrap(), b"PREEXISTING");
        assert!(parses_as_cia(&existing));
    }

    #[tokio::test]
    async fn cdn_to_cia_recursive_rename_writes_numbered_sibling() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_cdn_title(&root.join("title_a"), 0x0004000000030000);
        let existing = root.join("title_a.cia");
        std::fs::write(&existing, b"PREEXISTING").unwrap();

        let opts = recursive_opts(root.to_path_buf(), ConflictPolicy::Rename);
        convert_cdn_to_cia(opts, &NoProgress, &NoProgress)
            .await
            .unwrap();

        assert_eq!(std::fs::read(&existing).unwrap(), b"PREEXISTING");
        let renamed = root.join("title_a (1).cia");
        assert!(renamed.exists(), "missing {}", renamed.display());
        assert!(parses_as_cia(&renamed));
    }

    #[tokio::test]
    async fn cdn_to_cia_recursive_skips_non_cdn_subfolder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_cdn_title(&root.join("title_a"), 0x0004000000030000);
        write_cdn_title(&root.join("title_b"), 0x0004000000030001);
        let junk = root.join("not_cdn");
        std::fs::create_dir_all(&junk).unwrap();
        std::fs::write(junk.join("readme.txt"), b"x").unwrap();

        let opts = recursive_opts(root.to_path_buf(), ConflictPolicy::Error);
        convert_cdn_to_cia(opts, &NoProgress, &NoProgress)
            .await
            .unwrap();

        assert!(root.join("title_a.cia").exists());
        assert!(root.join("title_b.cia").exists());
        assert!(!root.join("not_cdn.cia").exists());
    }

    #[tokio::test]
    async fn cdn_to_cia_recursive_community_layout_variant() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = root.join("title_a");
        std::fs::create_dir_all(&dir).unwrap();

        let content: Vec<u8> = (0..0x400u32).map(|i| i as u8).collect();
        let mut hasher = Sha256::new();
        hasher.update(&content);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&hasher.finalize());
        std::fs::write(dir.join("00000000"), &content).unwrap();

        let tmd = make_tmd(0x0004000000030000, vec![(0, 0, content.clone(), hash)]);
        let mut tmd_buf = Vec::new();
        append_be(&mut tmd_buf, &tmd);
        append_be(&mut tmd_buf, &make_cert(b"CP0000000b", 0xBB));
        append_be(&mut tmd_buf, &make_cert(b"CA00000003", 0xAA));
        std::fs::write(dir.join("tmd.1029"), &tmd_buf).unwrap();

        let ticket = make_ticket(0x0004000000030000);
        let mut tik_buf = Vec::new();
        append_be(&mut tik_buf, &ticket);
        append_be(&mut tik_buf, &make_cert(b"XS0000000c", 0xCC));
        std::fs::write(dir.join("title.tik"), &tik_buf).unwrap();

        let opts = recursive_opts(root.to_path_buf(), ConflictPolicy::Error);
        convert_cdn_to_cia(opts, &NoProgress, &NoProgress)
            .await
            .unwrap();

        let out = root.join("title_a.cia");
        assert!(out.exists(), "missing {}", out.display());
        assert!(parses_as_cia(&out));
    }

    fn is_ctr_cancelled(err: &anyhow::Error) -> bool {
        err.chain()
            .any(|c| matches!(c.downcast_ref::<NintendoCTRError>(), Some(NintendoCTRError::Cancelled)))
    }

    struct CancelAfter {
        token: CancelToken,
        remaining: std::sync::atomic::AtomicUsize,
    }

    impl CancelAfter {
        fn new(token: CancelToken, after: usize) -> Self {
            Self {
                token,
                remaining: std::sync::atomic::AtomicUsize::new(after),
            }
        }
    }

    impl ProgressReporter for CancelAfter {
        fn start(&self, _: u64, _: &str) {}
        fn inc(&self, _: u64) {
            use std::sync::atomic::Ordering;
            if self.remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
                self.token.cancel();
            }
        }
        fn finish(&self) {}
    }

    fn ncch_scratch_present(dir: &Path) -> bool {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("ncch"))
    }

    #[tokio::test]
    async fn decrypt_cancel_before_start_leaves_no_output() {
        use crate::nintendo::ctr::test_fixtures::synth_encrypted_cia_multi_content;

        let (tmp, input, _) = synth_encrypted_cia_multi_content(&[0x0000_0000u32, 0x0000_0001u32]);
        let output = tmp.path().join("decrypted.cia");

        let token = CancelToken::new();
        token.cancel();
        let result = decrypt_rom_cancellable(&input, &output, &NoProgress, token).await;

        let err = result.expect_err("a pre-cancelled token must abort the decrypt");
        assert!(is_ctr_cancelled(&err), "error chain must carry the cancelled variant");
        assert!(!output.exists(), "no partial output");
        assert!(!scratch_output_path(&output).exists(), "no leftover temp");
        assert!(!ncch_scratch_present(tmp.path()), "no leftover .ncch scratch");
    }

    #[tokio::test]
    async fn decrypt_leaves_only_final_output_no_scratch() {
        use crate::nintendo::ctr::test_fixtures::synth_encrypted_cia_multi_content;

        let dir = tempfile::tempdir().unwrap();
        let (src_tmp, input, _) =
            synth_encrypted_cia_multi_content(&[0x0000_0000u32, 0x0000_ABCDu32]);
        let input2 = dir.path().join("game.cia");
        std::fs::copy(&input, &input2).unwrap();
        drop(src_tmp);

        let output = dir.path().join("game.decrypted.cia");
        decrypt_rom_cancellable(&input2, &output, &NoProgress, CancelToken::new())
            .await
            .unwrap();

        assert!(output.exists(), "final output must exist");
        assert!(parses_as_cia(&output), "final output is a valid CIA");
        assert!(!ncch_scratch_present(dir.path()), "no .ncch scratch left behind");

        let leftover_tmp = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.path().extension().and_then(|s| s.to_str()) == Some("tmp"));
        assert!(!leftover_tmp, "no .tmp scratch left behind");

        let entries: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries.len(),
            2,
            "only the input and the final output remain: {entries:?}"
        );
    }

    #[tokio::test]
    async fn decrypt_cancel_after_completion_succeeds() {
        use crate::nintendo::ctr::test_fixtures::synth_encrypted_cia_multi_content;

        let (tmp, input, _) = synth_encrypted_cia_multi_content(&[0x0000_0000u32, 0x0000_0001u32]);
        let output = tmp.path().join("decrypted.cia");

        let token = CancelToken::new();
        decrypt_rom_cancellable(&input, &output, &NoProgress, token.clone())
            .await
            .expect("decrypt must succeed with an uncancelled token");
        token.cancel();

        assert!(output.exists(), "output survives a post-completion cancel");
        assert!(parses_as_cia(&output), "decrypted output is a valid CIA");
        assert!(!scratch_output_path(&output).exists(), "no leftover temp");
        assert!(!ncch_scratch_present(tmp.path()), "no leftover .ncch scratch");
    }

    #[tokio::test]
    async fn decrypt_force_overwrite_preexisting_survives_cancel() {
        use crate::nintendo::ctr::test_fixtures::synth_encrypted_cia_multi_content;

        let (tmp, input, _) = synth_encrypted_cia_multi_content(&[0x0000_0000u32, 0x0000_0001u32]);
        let output = tmp.path().join("decrypted.cia");
        let original = b"do not destroy me".to_vec();
        std::fs::write(&output, &original).unwrap();

        let token = CancelToken::new();
        token.cancel();
        let result = decrypt_rom_cancellable(&input, &output, &NoProgress, token).await;

        let err = result.expect_err("a pre-cancelled token must abort the decrypt");
        assert!(is_ctr_cancelled(&err));
        assert_eq!(std::fs::read(&output).unwrap(), original);
        assert!(!scratch_output_path(&output).exists(), "no leftover temp");
    }

    #[tokio::test]
    async fn decrypt_batch_cancel_before_start_leaves_no_output() {
        use crate::nintendo::ctr::test_fixtures::synth_encrypted_cia_multi_content;

        let dir = tempfile::tempdir().unwrap();
        let (a_tmp, a_in, _) = synth_encrypted_cia_multi_content(&[0x0000_0000u32]);
        let (b_tmp, b_in, _) = synth_encrypted_cia_multi_content(&[0x0000_0001u32]);
        std::fs::copy(&a_in, dir.path().join("a.cia")).unwrap();
        std::fs::copy(&b_in, dir.path().join("b.cia")).unwrap();
        drop(a_tmp);
        drop(b_tmp);

        let token = CancelToken::new();
        token.cancel();
        let result = decrypt_rom_batch_cancellable(
            dir.path(),
            None,
            &NoProgress,
            &NoProgress,
            None,
            token,
        )
        .await;

        let err = result.expect_err("a pre-cancelled token must abort the batch");
        assert!(is_ctr_cancelled(&err), "error chain must carry the cancelled variant");
        assert!(!dir.path().join("a.decrypted.cia").exists());
        assert!(!dir.path().join("b.decrypted.cia").exists());
    }

    #[tokio::test]
    async fn decrypt_batch_cancel_mid_stops_remaining() {
        use crate::nintendo::ctr::test_fixtures::synth_encrypted_cia_multi_content;

        let dir = tempfile::tempdir().unwrap();
        let (a_tmp, a_in, _) = synth_encrypted_cia_multi_content(&[0x0000_0000u32]);
        let (b_tmp, b_in, _) = synth_encrypted_cia_multi_content(&[0x0000_0001u32]);
        std::fs::copy(&a_in, dir.path().join("a.cia")).unwrap();
        std::fs::copy(&b_in, dir.path().join("b.cia")).unwrap();
        drop(a_tmp);
        drop(b_tmp);

        let token = CancelToken::new();
        let cancel_after_first = CancelAfter::new(token.clone(), 1);
        let result = decrypt_rom_batch_cancellable(
            dir.path(),
            None,
            &NoProgress,
            &cancel_after_first,
            None,
            token,
        )
        .await;

        let err = result.expect_err("cancelling mid-batch must abort the run");
        assert!(is_ctr_cancelled(&err));
        let produced = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .ends_with(".decrypted.cia")
            })
            .count();
        assert_eq!(produced, 1, "only the first file completes before cancel");
    }

    #[test]
    fn decrypt_path_cia() {
        assert_eq!(
            derive_decrypted_path(Path::new("game.cia")),
            PathBuf::from("game.decrypted.cia"),
        );
    }

    #[test]
    fn decrypt_path_3ds() {
        assert_eq!(
            derive_decrypted_path(Path::new("game.3ds")),
            PathBuf::from("game.decrypted.3ds"),
        );
    }

    #[test]
    fn decrypt_path_cci() {
        assert_eq!(
            derive_decrypted_path(Path::new("game.cci")),
            PathBuf::from("game.decrypted.cci"),
        );
    }

    #[test]
    fn decrypt_path_cxi() {
        assert_eq!(
            derive_decrypted_path(Path::new("game.cxi")),
            PathBuf::from("game.decrypted.cxi"),
        );
    }

    #[test]
    fn decrypt_path_preserves_directory() {
        assert_eq!(
            derive_decrypted_path(Path::new("/roms/game.cia")),
            PathBuf::from("/roms/game.decrypted.cia"),
        );
    }

    #[test]
    fn decrypt_path_no_extension() {
        assert_eq!(
            derive_decrypted_path(Path::new("game")),
            PathBuf::from("game.decrypted"),
        );
    }
}
