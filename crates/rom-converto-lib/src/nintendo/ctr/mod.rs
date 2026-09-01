//! Format detection and top-level dispatch for the 3DS family: CDN-to-CIA
//! assembly, decryption, conversion between CIA and CCI, verification, and
//! the Z3DS compression pipeline.

use crate::nintendo::ctr::cia::{CiaWriteArgs, decrypt_from_encrypted_cia, write_cia};
use crate::nintendo::ctr::constants::{
    NCCH_MAGIC_OFFSET, TICKET_SIG_BODY_OFFSET, TICKET_TITLE_ID_OFFSET, TICKET_TITLE_KEY_OFFSET,
    TICKET_TITLE_VERSION_OFFSET,
};
use crate::nintendo::ctr::decrypt::cia::{parse_and_decrypt_ncch, parse_and_decrypt_ncsd};
use crate::nintendo::ctr::decrypt::util::{cbc_decrypt, derive_title_key_from_ticket, gen_iv};
pub use crate::nintendo::ctr::encrypt::{
    derive_encrypted_path, encrypt_rom, encrypt_rom_batch_cancellable, encrypt_rom_cancellable,
};
use crate::nintendo::ctr::error::NintendoCTRError;
use crate::nintendo::ctr::models::cia::CIA_HEADER_SIZE;
use crate::nintendo::ctr::models::ticket::Ticket;
use crate::nintendo::ctr::models::title_metadata::TitleMetadata;
use crate::nintendo::ctr::title_key::generate_title_key;
use crate::nintendo::ctr::util::fs::{find_title_file, find_tmd_file};
use crate::nintendo::ctr::z3ds::models::underlying_magic;
use crate::nintendo::ctr::z3ds::{compress_rom_cancellable, derive_compressed_path};
use crate::util::{
    CancelToken, ConflictPolicy, ConflictResolution, ProgressReporter, resolve_conflict,
    scratch_output_path,
};
use anyhow::Result;
use binrw::BinRead;
use log::{debug, info, warn};
use sha2::{Digest, Sha256};
use std::io::{Cursor, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use tempfile::TempPath;
use tokio::fs;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter};

mod cia;
mod constants;
pub mod convert;
mod decrypt;
mod encrypt;
/// Error types for CTR (3DS) operations.
pub mod error;
pub mod exefs;
pub mod info;
/// Binary-format structs for CTR container and metadata layouts (CIA, NCCH,
/// NCSD, ticket, TMD, SMDH, certificates).
pub mod models;
pub mod seed;
#[cfg(test)]
pub(crate) mod test_fixtures;
/// Derives and encrypts 3DS title keys from a title ID.
pub mod title_key;
mod util;
pub mod verify;
/// Custom Z3DS compression container format and its (de)compression pipeline.
pub mod z3ds;

/// Options controlling a CDN-directory-to-CIA conversion.
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

/// Derives the output path for a decrypted ROM by inserting `.decrypted`
/// before the file extension.
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

const FORGED_KEY_VERIFY_BUF: usize = 4 * 1024 * 1024;

/// Decrypts a CIA file to `output`, deriving the title key from its own ticket.
pub async fn decrypt_cia(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
) -> Result<()> {
    decrypt_cia_cancellable(input, output, progress, CancelToken::new()).await
}

/// Like [`decrypt_cia`] but observes `cancel` during decryption.
pub async fn decrypt_cia_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<()> {
    let tmp = scratch_output_path(output)?;
    let out = File::create(&tmp).await?;
    let mut out = BufWriter::new(out);

    if let Err(err) = decrypt_from_encrypted_cia(input, &mut out, progress, &cancel).await {
        drop(out);
        fs::remove_file(&tmp).await.ok();
        return Err(err);
    }

    out.flush().await?;
    drop(out);
    crate::util::publish_temp(tmp, output, true)?;

    info!("Decrypted CIA file");

    Ok(())
}

/// Decrypts a CIA, NCSD (`.3ds`/`.cci`), or standalone NCCH (`.cxi`) ROM to
/// `output`, detecting the format from its magic bytes.
pub async fn decrypt_rom(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
) -> Result<()> {
    decrypt_rom_cancellable(input, output, progress, CancelToken::new()).await
}

/// Like [`decrypt_rom`] but observes `cancel` throughout.
pub async fn decrypt_rom_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<()> {
    let file_size = tokio::fs::metadata(input).await?.len();
    progress.start(file_size, "Decrypting");

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
                "unrecognized format: no NCSD/NCCH magic at 0x100 and not a CIA file"
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
    let tmp = scratch_output_path(output)?;

    // The verbatim copy carries the NCSD header, inter-partition gaps, and any
    // plain partitions; parse_and_decrypt_ncsd overwrites each NCCH partition
    // region in place, so the decrypt streams straight into the final temp
    // without per-partition scratch files.
    let result = async {
        fs::copy(input, &tmp).await?;
        let mut out = fs::OpenOptions::new()
            .write(true)
            .read(true)
            .open(&tmp)
            .await?;
        parse_and_decrypt_ncsd(input, &mut out, None, progress, cancel).await?;
        out.flush().await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Err(err) = result {
        fs::remove_file(&tmp).await.ok();
        return Err(err);
    }

    crate::util::publish_temp(tmp, output, true)?;

    info!("Decrypted NCSD file");
    Ok(())
}

async fn decrypt_ncch_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<()> {
    let tmp = scratch_output_path(output)?;

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

    crate::util::publish_temp(tmp, output, true)?;

    info!("Decrypted NCCH file");
    Ok(())
}

/// Synthesizes a ticket (`cetk`) for a CDN title dump, deriving the title
/// key from the title ID read out of its TMD.
pub async fn generate_ticket_from_cdn(cdn_dir: &Path, output: &Path) -> Result<()> {
    generate_ticket_from_cdn_cancellable(cdn_dir, output, &CancelToken::new()).await
}

/// Like [`generate_ticket_from_cdn`] but observes `cancel`.
pub async fn generate_ticket_from_cdn_cancellable(
    cdn_dir: &Path,
    output: &Path,
    cancel: &CancelToken,
) -> Result<()> {
    generate_ticket_from_cdn_with_publish(cdn_dir, output, cancel, true).await
}

pub(crate) async fn generate_ticket_from_cdn_with_publish(
    cdn_dir: &Path,
    output: &Path,
    cancel: &CancelToken,
    overwrite: bool,
) -> Result<()> {
    check_cancel(cancel)?;
    let tmd_path = find_tmd_file(cdn_dir).await?;
    debug!("Found TMD file at {}", tmd_path.display());

    let mut ticket_metadata_data = Cursor::new(fs::read(&tmd_path).await?);
    check_cancel(cancel)?;
    let title_metadata = TitleMetadata::read(&mut ticket_metadata_data)?;

    let title_id_str = format!("{:016X}", title_metadata.header.title_id);

    let title_key = generate_title_key(&title_id_str, None)?;

    const CETK_STRING_TEMPLATE: &str = "00010004d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0d15ea5e0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000526f6f742d434130303030303030332d585330303030303030630000000000000000000000000000000000000000000000000000000000000000000000000000feedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedface010000eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee00000000000000000000000000dddddddddddddddd00001111000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010014000000ac000000140001001400000000000000280000000100000084000000840003000000000000ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010004919ebe464ad0f552cd1b72e7884910cf55a9f02e50789641d896683dc005bd0aea87079d8ac284c675065f74c8bf37c88044409502a022980bb8ad48383f6d28a79de39626ccb2b22a0f19e41032f094b39ff0133146dec8f6c1a9d55cd28d9e1c47b3d11f4f5426c2c780135a2775d3ca679bc7e834f0e0fb58e68860a71330fc95791793c8fba935a7a6908f229dee2a0ca6b9b23b12d495a6fe19d0d72648216878605a66538dbf376899905d3445fc5c727a0e13e0e2c8971c9cfa6c60678875732a4e75523d2f562f12aabd1573bf06c94054aefa81a71417af9a4a066d0ffc5ad64bab28b1ff60661f4437d49e1e0d9412eb4bcacf4cfd6a3408847982000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000526f6f742d43413030303030303033000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000158533030303030303063000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000137a0894ad505bb6c67e2e5bdd6a3bec43d910c772e9cc290da58588b77dcc11680bb3e29f4eabbb26e98c2601985c041bb14378e689181aad770568e928a2b98167ee3e10d072beef1fa22fa2aa3e13f11e1836a92a4281ef70aaf4e462998221c6fbb9bdd017e6ac590494e9cea9859ceb2d2a4c1766f2c33912c58f14a803e36fccdcccdc13fd7ae77c7a78d997e6acc35557e0d3e9eb64b43c92f4c50d67a602deb391b06661cd32880bd64912af1cbcb7162a06f02565d3b0ece4fcecddae8a4934db8ee67f3017986221155d131c6c3f09ab1945c206ac70c942b36f49a1183bcd78b6e4b47c6c5cac0f8d62f897c6953dd12f28b70c5b7df751819a9834652625000100010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000010003704138efbbbda16a987dd901326d1c9459484c88a2861b91a312587ae70ef6237ec50e1032dc39dde89a96a8e859d76a98a6e7e36a0cfe352ca893058234ff833fcb3b03811e9f0dc0d9a52f8045b4b2f9411b67a51c44b5ef8ce77bd6d56ba75734a1856de6d4bed6d3a242c7c8791b3422375e5c779abf072f7695efa0f75bcb83789fc30e3fe4cc8392207840638949c7f688565f649b74d63d8d58ffadda571e9554426b1318fc468983d4c8a5628b06b6fc5d507c13e7a18ac1511eb6d62ea5448f83501447a9afb3ecc2903c9dd52f922ac9acdbef58c6021848d96e208732d3d1d9d9ea440d91621c7a99db8843c59c1f2e2c7d9b577d512c166d6f7e1aad4a774a37447e78fe2021e14a95d112a068ada019f463c7a55685aabb6888b9246483d18b9c806f474918331782344a4b8531334b26303263d9d2eb4f4bb99602b352f6ae4046c69a5e7e8e4a18ef9bc0a2ded61310417012fd824cc116cfb7c4c1f7ec7177a17446cbde96f3edd88fcd052f0b888a45fdaf2b631354f40d16e5fa9c2c4eda98e798d15e6046dc5363f3096b2c607a9d8dd55b1502a6ac7d3cc8d8c575998e7d796910c804c495235057e91ecd2637c9c1845151ac6b9a0490ae3ec6f47740a0db0ba36d075956cee7354ea3e9a4f2720b26550c7d394324bc0cb7e9317d8a8661f42191ff10b08256ce3fd25b745e5194906b4d61cb4c2e000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000526f6f7400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001434130303030303030330000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000007be8ef6cb279c9e2eee121c6eaf44ff639f88f078b4b77ed9f9560b0358281b50e55ab721115a177703c7a30fe3ae9ef1c60bc1d974676b23a68cc04b198525bc968f11de2db50e4d9e7f071e562dae2092233e9d363f61dd7c19ff3a4a91e8f6553d471dd7b84b9f1b8ce7335f0f5540563a1eab83963e09be901011f99546361287020e9cc0dab487f140d6626a1836d27111f2068de4772149151cf69c61ba60ef9d949a0f71f5499f2d39ad28c7005348293c431ffbd33f6bca60dc7195ea2bcc56d200baf6d06d09c41db8de9c720154ca4832b69c08c69cd3b073a0063602f462d338061a5ea6c915cd5623579c3eb64ce44ef586d14baaa8834019b3eebeed3790001000100000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

    let mut bytes = hex::decode(CETK_STRING_TEMPLATE)?;
    check_cancel(cancel)?;

    let title_key_bytes = hex::decode(&title_key)?;
    let title_key_offset = (TICKET_SIG_BODY_OFFSET + TICKET_TITLE_KEY_OFFSET) as usize;
    bytes[title_key_offset..title_key_offset + title_key_bytes.len()]
        .copy_from_slice(&title_key_bytes);

    let title_id_offset = (TICKET_SIG_BODY_OFFSET + TICKET_TITLE_ID_OFFSET) as usize;
    bytes[title_id_offset..title_id_offset + 8]
        .copy_from_slice(&title_metadata.header.title_id.to_be_bytes());

    let title_version_offset = (TICKET_SIG_BODY_OFFSET + TICKET_TITLE_VERSION_OFFSET) as usize;
    bytes[title_version_offset..title_version_offset + 2]
        .copy_from_slice(&title_metadata.header.title_version.to_be_bytes());

    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    check_cancel(cancel)?;
    crate::util::publish_temp(file.into_temp_path(), output, overwrite)?;

    info!("Created ticket at {}", output.display());

    Ok(())
}

fn check_cancel(cancel: &CancelToken) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(NintendoCTRError::Cancelled.into());
    }
    Ok(())
}

/// Assembles a CIA from a CDN title dump, or from every subdirectory of one
/// when `opts.recursive` is set.
pub async fn convert_cdn_to_cia(
    opts: CdnToCiaOptions,
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
) -> Result<()> {
    convert_cdn_to_cia_cancellable(opts, progress, total_progress, CancelToken::new()).await
}

/// Like [`convert_cdn_to_cia`] but observes `cancel`.
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

        total_progress.start(count, &format!("Processing {count} directories"));

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

            if let Err(err) = convert_cdn_to_cia_single(opts_clone, progress, cancel.clone()).await
            {
                if err
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|err| err.kind() == std::io::ErrorKind::InvalidInput)
                {
                    total_progress.finish();
                    return Err(err);
                }
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

    let final_path = if opts.compress {
        derive_compressed_path(&output)
    } else {
        output.clone()
    };
    let cdn_dir = &opts.cdn_dir;
    if opts.cleanup && path_is_within(&final_path, cdn_dir)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "output {} is inside CDN directory {}; cleanup would delete it",
                final_path.display(),
                cdn_dir.display()
            ),
        )
        .into());
    }
    let final_output = match resolve_conflict(&final_path, opts.on_conflict)? {
        ConflictResolution::Skip => {
            info!("Skipped, output exists: {}", final_path.display());
            return Ok(());
        }
        ConflictResolution::Write(resolved) => resolved,
    };

    let (ticket_path, forged) = match find_title_file(cdn_dir).await {
        Ok(path) => (path, false),
        Err(err) => {
            if !opts.ensure_ticket_exists {
                return Err(err.into());
            }
            check_cancel(&cancel)?;
            let desired = cdn_dir.join("ticket.tik");
            let path = match resolve_conflict(&desired, opts.on_conflict)? {
                ConflictResolution::Skip => desired,
                ConflictResolution::Write(path) => {
                    generate_ticket_from_cdn_with_publish(
                        cdn_dir,
                        &path,
                        &cancel,
                        opts.on_conflict == ConflictPolicy::Overwrite,
                    )
                    .await?;
                    path
                }
            };
            debug!("Path for ticket file: {}", path.display());
            debug!("CDN Directory: {}", cdn_dir.display());
            (path, true)
        }
    };
    debug!("Found Ticket file at {}", ticket_path.display());

    let title_metadata_path = find_tmd_file(cdn_dir).await?;
    debug!("Found TMD file at {}", title_metadata_path.display());

    let mut ticket_metadata_data = Cursor::new(fs::read(&title_metadata_path).await?);
    let title_metadata = TitleMetadata::read(&mut ticket_metadata_data)?;
    let title_id = title_metadata.header.title_id;

    let ticket_bytes = fs::read(&ticket_path).await?;
    let ticket = Ticket::read(&mut Cursor::new(&ticket_bytes))?;

    // A ticket left on disk by an earlier `generate-cdn-ticket` run carries the
    // same derived key as one forged here, so key it off the key itself rather
    // than off who wrote the file. Only a ticket forged this run is ours to delete.
    let derived_title_key = hex::decode(generate_title_key(&format!("{title_id:016X}"), None)?)?;
    if ticket.ticket_data.title_key == derived_title_key
        && let Err(err) =
            verify_forged_title_key(cdn_dir, &title_metadata, &ticket_bytes, &cancel).await
    {
        if forged && let Err(remove_err) = fs::remove_file(&ticket_path).await {
            warn!(
                "Failed to remove forged ticket {}: {remove_err}",
                ticket_path.display()
            );
        }
        return Err(err);
    }

    debug!("Processing CIA conversion");

    let ticket_title_id = ticket.ticket_data.title_id;

    if ticket_title_id != title_id {
        warn!(
            "TICKET and TMD Title IDs do not match: TICKET=0x{ticket_title_id:016X}, TMD=0x{title_id:016X}"
        );
    }

    let encrypted = private_temp_path(&final_output, ".cia")?;
    let out = File::create(&encrypted).await?;
    let mut out_buffered = BufWriter::new(out);
    if let Err(err) = write_cia(
        &mut out_buffered,
        CiaWriteArgs {
            path: cdn_dir,
            tmd_path: &title_metadata_path,
            tmd: title_metadata,
            tik_path: &ticket_path,
            tik: ticket,
            progress,
            cancel: &cancel,
        },
    )
    .await
    {
        drop(out_buffered);
        return Err(err);
    }
    out_buffered.flush().await?;
    drop(out_buffered);
    let decrypted = if opts.decrypt {
        let decrypted = private_temp_path(&final_output, ".cia")?;
        decrypt_cia_cancellable(&encrypted, &decrypted, progress, cancel.clone()).await?;
        Some(decrypted)
    } else {
        None
    };

    if opts.compress {
        let output = decrypted.as_deref().unwrap_or(&encrypted);
        let compressed = private_temp_path(&final_output, ".zcia")?;
        compress_rom_cancellable(output, &compressed, None, false, progress, cancel).await?;
        publish_temp_path(compressed, &final_output, opts.on_conflict)?;
    } else {
        publish_temp_path(
            decrypted.unwrap_or(encrypted),
            &final_output,
            opts.on_conflict,
        )?;
        info!("Created CIA file {}", final_output.display());
    }

    if opts.cleanup {
        fs::remove_dir_all(cdn_dir).await?;

        debug!("Deleted CDN directory: {}", cdn_dir.display());
    }

    Ok(())
}

/// A forged ticket carries a title key derived from the title id, which only
/// matches titles Nintendo keyed the same way. Left unchecked, a wrong
/// derivation yields a CIA that installs and then fails to decrypt on-console,
/// so decrypt the cheapest encrypted content and hash it against the TMD.
async fn verify_forged_title_key(
    cdn_dir: &Path,
    tmd: &TitleMetadata,
    ticket_bytes: &[u8],
    cancel: &CancelToken,
) -> Result<()> {
    let Some(record) = tmd
        .content_chunk_records
        .iter()
        .filter(|record| {
            record.content_type.is_encrypted()
                && record.content_size >= 16
                && record.content_size % 16 == 0
                && cdn_dir.join(format!("{:08x}", record.content_id)).is_file()
        })
        .min_by_key(|record| record.content_size)
    else {
        warn!(
            "no encrypted content to verify the forged title key against for 0x{:016X}",
            tmd.header.title_id
        );
        return Ok(());
    };

    let content_path = cdn_dir.join(format!("{:08x}", record.content_id));
    let actual_size = fs::metadata(&content_path).await?.len();
    if actual_size != record.content_size {
        anyhow::bail!(
            "content file {} size mismatch: TMD declares {} bytes but file is {} bytes",
            content_path.display(),
            record.content_size,
            actual_size,
        );
    }

    let title_key = derive_title_key_from_ticket(&mut Cursor::new(ticket_bytes), 0)?;
    let mut file = File::open(&content_path).await?;
    let mut buf = vec![0u8; FORGED_KEY_VERIFY_BUF.min(record.content_size as usize)];
    let mut hasher = Sha256::new();
    let mut iv = gen_iv(record.content_index);
    let mut remaining = record.content_size;

    while remaining > 0 {
        check_cancel(cancel)?;
        let to_read = remaining.min(buf.len() as u64) as usize;
        file.read_exact(&mut buf[..to_read]).await?;
        // The next chunk chains off this chunk's last ciphertext block, which
        // in-place decryption is about to overwrite.
        let next_iv: [u8; 16] = buf[to_read - 16..to_read].try_into().expect("16 bytes");
        cbc_decrypt(&title_key, &iv, &mut buf[..to_read])?;
        iv = next_iv;
        hasher.update(&buf[..to_read]);
        remaining -= to_read as u64;
    }

    if hasher.finalize().as_slice() != record.hash.as_slice() {
        return Err(NintendoCTRError::ForgedTicketKeyMismatch(tmd.header.title_id).into());
    }

    Ok(())
}

fn private_temp_path(output: &Path, suffix: &str) -> std::io::Result<TempPath> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let path = tempfile::Builder::new()
        .prefix(".rom-converto-")
        .suffix(suffix)
        .tempfile_in(parent)?
        .into_temp_path();
    std::fs::remove_file(&path)?;
    Ok(path)
}

fn publish_temp_path(path: TempPath, output: &Path, policy: ConflictPolicy) -> std::io::Result<()> {
    crate::util::publish_temp(path, output, policy == ConflictPolicy::Overwrite)
}

fn path_is_within(path: &Path, directory: &Path) -> std::io::Result<bool> {
    let directory = directory.canonicalize()?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut ancestor = absolute.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "output path has no parent",
        )
    })?;
    let mut tail = vec![
        absolute
            .file_name()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "output path has no file name",
                )
            })?
            .to_os_string(),
    ];
    while !ancestor.exists() {
        if let Some(name) = ancestor.file_name() {
            tail.push(name.to_os_string());
        }
        ancestor = ancestor.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "output path has no parent",
            )
        })?;
    }
    let mut path = ancestor.canonicalize()?;
    for component in tail.into_iter().rev() {
        if component == ".." {
            path.pop();
        } else if component != "." {
            path.push(component);
        }
    }
    Ok(path.starts_with(directory))
}

/// Decrypts every supported ROM file (`.cia`, `.3ds`, `.cci`, `.cxi`) found
/// under `input_dir`.
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

/// Like [`decrypt_rom_batch`] but observes `cancel` between files.
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

    total_progress.start(
        roms.len() as u64,
        &format!("Decrypting {} files", roms.len()),
    );

    if let Some(dir) = output_dir {
        fs::create_dir_all(dir).await?;
    }

    for path in roms {
        if cancel.is_cancelled() {
            return Err(NintendoCTRError::Cancelled.into());
        }
        let output = crate::util::place_in_dir_mirrored(
            &derive_decrypted_path(&path),
            input_dir,
            output_dir,
        );
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

        let tmd = make_tmd(title_id, vec![(0, 0, content.clone(), hash)], false);
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

    fn single_opts(cdn_dir: PathBuf, output: PathBuf) -> CdnToCiaOptions {
        CdnToCiaOptions {
            cdn_dir,
            output: Some(output),
            cleanup: false,
            recursive: false,
            ensure_ticket_exists: false,
            decrypt: false,
            compress: false,
            output_dir: None,
            on_conflict: ConflictPolicy::Error,
        }
    }

    /// The plaintext title key a forged ticket for `title_id` carries, i.e. what
    /// `derive_title_key_from_ticket` recovers from the generated ticket.
    fn derived_plain_title_key(title_id: u64) -> [u8; 16] {
        let key = crate::nintendo::ctr::title_key::generate_key(
            &format!("{title_id:016X}"),
            crate::nintendo::ctr::constants::CTR_DEFAULT_TITLE_KEY_PASSWORD,
        )
        .unwrap();
        hex::decode(key).unwrap().try_into().unwrap()
    }

    fn cbc_encrypt(key: &[u8; 16], iv: &[u8; 16], plain: &[u8]) -> Vec<u8> {
        use aes::Aes128;
        use block_padding::NoPadding;
        use cbc::cipher::{BlockModeEncrypt, KeyIvInit};

        let len = plain.len();
        let mut buf = plain.to_vec();
        buf.resize(len + 16, 0);
        cbc::Encryptor::<Aes128>::new_from_slices(key, iv)
            .unwrap()
            .encrypt_padded::<NoPadding>(&mut buf, len)
            .unwrap()
            .to_vec()
    }

    fn parses_as_cia(path: &Path) -> bool {
        let bytes = std::fs::read(path).unwrap();
        CiaFile::read_options(&mut Cursor::new(&bytes), Endian::Little, ()).is_ok()
    }

    #[tokio::test]
    async fn cancelled_ticket_generation_preserves_existing_output() {
        let tmp = tempfile::tempdir().unwrap();
        let output = tmp.path().join("ticket.tik");
        std::fs::write(&output, b"existing").unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();

        let err = generate_ticket_from_cdn_cancellable(tmp.path(), &output, &cancel)
            .await
            .unwrap_err();

        assert!(matches!(
            err.downcast_ref::<NintendoCTRError>(),
            Some(NintendoCTRError::Cancelled)
        ));
        assert_eq!(std::fs::read(output).unwrap(), b"existing");
    }

    #[tokio::test]
    async fn generated_ticket_preserves_title_key_containing_version_placeholder() {
        let title_id = 0x0004008C001A6B00u64;

        let tmp = tempfile::tempdir().unwrap();
        let cdn_dir = tmp.path();
        let mut tmd = make_tmd(title_id, vec![], false);
        tmd.header.title_version = 0;
        let mut tmd_buf = Vec::new();
        append_be(&mut tmd_buf, &tmd);
        std::fs::write(cdn_dir.join("tmd"), &tmd_buf).unwrap();

        let output = cdn_dir.join("ticket.tik");
        generate_ticket_from_cdn(cdn_dir, &output).await.unwrap();
        let bytes = std::fs::read(&output).unwrap();

        let title_id_str = format!("{title_id:016X}");
        let expected_key = generate_title_key(&title_id_str, None).unwrap();
        let expected_key_bytes = hex::decode(&expected_key).unwrap();

        assert_eq!(&bytes[0x1BF..0x1CF], expected_key_bytes.as_slice());
        assert_eq!(&bytes[0x1DC..0x1E4], &title_id.to_be_bytes());
        assert_eq!(&bytes[0x1E6..0x1E8], &0u16.to_be_bytes());
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

        let tmd = make_tmd(
            0x0004000000030000,
            vec![(0, 0, content.clone(), hash)],
            false,
        );
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

    #[tokio::test]
    async fn cdn_to_cia_dsiware_with_ticket_flag_forges_ticket() {
        let tmp = tempfile::tempdir().unwrap();
        let cdn_dir = tmp.path().join("dsiware");
        std::fs::create_dir_all(&cdn_dir).unwrap();

        let make_content = |seed: u8| -> (Vec<u8>, [u8; 32]) {
            let data: Vec<u8> = (0..0x400u32)
                .map(|i| (i as u8).wrapping_add(seed))
                .collect();
            let mut hasher = Sha256::new();
            hasher.update(&data);
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&hasher.finalize());
            (data, hash)
        };
        let (c0, h0) = make_content(0);
        let (c2, h2) = make_content(2);
        std::fs::write(cdn_dir.join("00000000"), &c0).unwrap();
        std::fs::write(cdn_dir.join("00000002"), &c2).unwrap();

        let title_id = 0x0004800400000000u64;
        let tmd = make_tmd(title_id, vec![(0, 0, c0, h0), (2, 1, c2, h2)], false);
        let mut tmd_buf = Vec::new();
        append_be(&mut tmd_buf, &tmd);
        append_be(&mut tmd_buf, &make_cert(b"CP0000000b", 0xBB));
        append_be(&mut tmd_buf, &make_cert(b"CA00000003", 0xAA));
        std::fs::write(cdn_dir.join("tmd.0"), &tmd_buf).unwrap();

        let output = tmp.path().join("dsiware.cia");
        let mut opts = single_opts(cdn_dir, output.clone());
        opts.ensure_ticket_exists = true;
        convert_cdn_to_cia(opts, &NoProgress, &NoProgress)
            .await
            .unwrap();

        assert!(output.exists(), "missing {}", output.display());
        assert!(parses_as_cia(&output));
    }

    #[tokio::test]
    async fn cdn_to_cia_forged_ticket_rejects_undecryptable_content() {
        let tmp = tempfile::tempdir().unwrap();
        let cdn_dir = tmp.path().join("dsiware");
        std::fs::create_dir_all(&cdn_dir).unwrap();

        let content: Vec<u8> = (0..0x400u32).map(|i| i as u8).collect();
        let mut hasher = Sha256::new();
        hasher.update(&content);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&hasher.finalize());
        std::fs::write(cdn_dir.join("00000000"), &content).unwrap();

        let title_id = 0x0004800400000000u64;
        let tmd = make_tmd(title_id, vec![(0, 0, content, hash)], true);
        let mut tmd_buf = Vec::new();
        append_be(&mut tmd_buf, &tmd);
        append_be(&mut tmd_buf, &make_cert(b"CP0000000b", 0xBB));
        append_be(&mut tmd_buf, &make_cert(b"CA00000003", 0xAA));
        std::fs::write(cdn_dir.join("tmd"), &tmd_buf).unwrap();

        let output = tmp.path().join("dsiware.cia");
        let mut opts = single_opts(cdn_dir.clone(), output.clone());
        opts.ensure_ticket_exists = true;
        let err = convert_cdn_to_cia(opts, &NoProgress, &NoProgress)
            .await
            .expect_err("derived title key does not decrypt the content");

        assert!(matches!(
            err.downcast_ref::<NintendoCTRError>(),
            Some(NintendoCTRError::ForgedTicketKeyMismatch(id)) if *id == title_id
        ));
        assert!(!output.exists(), "must not publish {}", output.display());
        assert!(
            !cdn_dir.join("ticket.tik").exists(),
            "forged ticket must be removed after verification failure"
        );
    }

    #[tokio::test]
    async fn cdn_to_cia_forged_ticket_accepts_content_encrypted_with_derived_key() {
        let tmp = tempfile::tempdir().unwrap();
        let cdn_dir = tmp.path().join("dsiware");
        std::fs::create_dir_all(&cdn_dir).unwrap();

        let title_id = 0x0004800400000000u64;
        // Over one verify buffer, so the CBC chaining crosses a chunk boundary.
        let plain: Vec<u8> = (0..FORGED_KEY_VERIFY_BUF + 32)
            .map(|i| (i as u8).wrapping_mul(37))
            .collect();
        let mut hasher = Sha256::new();
        hasher.update(&plain);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&hasher.finalize());

        let encrypted = cbc_encrypt(&derived_plain_title_key(title_id), &gen_iv(0), &plain);
        std::fs::write(cdn_dir.join("00000000"), &encrypted).unwrap();

        let tmd = make_tmd(title_id, vec![(0, 0, plain, hash)], true);
        let mut tmd_buf = Vec::new();
        append_be(&mut tmd_buf, &tmd);
        append_be(&mut tmd_buf, &make_cert(b"CP0000000b", 0xBB));
        append_be(&mut tmd_buf, &make_cert(b"CA00000003", 0xAA));
        std::fs::write(cdn_dir.join("tmd"), &tmd_buf).unwrap();

        let output = tmp.path().join("dsiware.cia");
        let mut opts = single_opts(cdn_dir, output.clone());
        opts.ensure_ticket_exists = true;
        convert_cdn_to_cia(opts, &NoProgress, &NoProgress)
            .await
            .unwrap();

        assert!(output.exists(), "missing {}", output.display());
        assert!(parses_as_cia(&output));
    }

    #[tokio::test]
    async fn cdn_to_cia_preexisting_forged_ticket_is_verified_and_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let cdn_dir = tmp.path().join("dsiware");
        std::fs::create_dir_all(&cdn_dir).unwrap();

        let content: Vec<u8> = (0..0x400u32).map(|i| i as u8).collect();
        let mut hasher = Sha256::new();
        hasher.update(&content);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&hasher.finalize());
        std::fs::write(cdn_dir.join("00000000"), &content).unwrap();

        let title_id = 0x0004800400000000u64;
        let tmd = make_tmd(title_id, vec![(0, 0, content, hash)], true);
        let mut tmd_buf = Vec::new();
        append_be(&mut tmd_buf, &tmd);
        append_be(&mut tmd_buf, &make_cert(b"CP0000000b", 0xBB));
        append_be(&mut tmd_buf, &make_cert(b"CA00000003", 0xAA));
        std::fs::write(cdn_dir.join("tmd"), &tmd_buf).unwrap();

        let ticket_path = cdn_dir.join("ticket.tik");
        generate_ticket_from_cdn(&cdn_dir, &ticket_path)
            .await
            .unwrap();

        let output = tmp.path().join("dsiware.cia");
        let opts = single_opts(cdn_dir, output.clone());
        let err = convert_cdn_to_cia(opts, &NoProgress, &NoProgress)
            .await
            .expect_err("a ticket left by generate-cdn-ticket must still be verified");

        assert!(matches!(
            err.downcast_ref::<NintendoCTRError>(),
            Some(NintendoCTRError::ForgedTicketKeyMismatch(id)) if *id == title_id
        ));
        assert!(!output.exists(), "must not publish {}", output.display());
        assert!(
            ticket_path.exists(),
            "a ticket this run did not forge must be left in place"
        );
    }

    #[tokio::test]
    async fn cdn_to_cia_without_ticket_or_flag_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let cdn_dir = tmp.path().join("title");
        std::fs::create_dir_all(&cdn_dir).unwrap();

        let content: Vec<u8> = (0..0x400u32).map(|i| i as u8).collect();
        let mut hasher = Sha256::new();
        hasher.update(&content);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&hasher.finalize());
        std::fs::write(cdn_dir.join("00000000"), &content).unwrap();

        let tmd = make_tmd(0x0004000000030000, vec![(0, 0, content, hash)], false);
        let mut tmd_buf = Vec::new();
        append_be(&mut tmd_buf, &tmd);
        std::fs::write(cdn_dir.join("tmd"), &tmd_buf).unwrap();

        let output = tmp.path().join("title.cia");
        let opts = single_opts(cdn_dir, output);
        let err = convert_cdn_to_cia(opts, &NoProgress, &NoProgress)
            .await
            .expect_err("non-DSiWare without a ticket must not forge one");

        assert!(matches!(
            err.downcast_ref::<NintendoCTRError>(),
            Some(NintendoCTRError::NoTitleFileFound(_))
        ));
    }

    #[tokio::test]
    async fn compressed_cdn_failure_preserves_existing_outputs() {
        let tmp = tempfile::tempdir().unwrap();
        let cdn_dir = tmp.path().join("title");
        write_cdn_title(&cdn_dir, 0x0004000000030000);
        let cia = tmp.path().join("title.cia");
        let zcia = tmp.path().join("title.zcia");
        std::fs::write(&cia, b"PREEXISTING CIA").unwrap();
        std::fs::write(&zcia, b"PREEXISTING ZCIA").unwrap();

        let mut opts = single_opts(cdn_dir, cia.clone());
        opts.compress = true;
        opts.on_conflict = ConflictPolicy::Overwrite;
        convert_cdn_to_cia(opts, &NoProgress, &NoProgress)
            .await
            .expect_err("fixture content has no decryptable NCCH");

        assert_eq!(std::fs::read(cia).unwrap(), b"PREEXISTING CIA");
        assert_eq!(std::fs::read(zcia).unwrap(), b"PREEXISTING ZCIA");
    }

    #[tokio::test]
    async fn decrypted_cdn_failure_preserves_existing_output() {
        let tmp = tempfile::tempdir().unwrap();
        let cdn_dir = tmp.path().join("title");
        write_cdn_title(&cdn_dir, 0x0004000000030000);
        let output = tmp.path().join("title.cia");
        std::fs::write(&output, b"PREEXISTING CIA").unwrap();

        let mut opts = single_opts(cdn_dir, output.clone());
        opts.decrypt = true;
        opts.on_conflict = ConflictPolicy::Overwrite;
        convert_cdn_to_cia(opts, &NoProgress, &NoProgress)
            .await
            .expect_err("fixture content has no decryptable NCCH");

        assert_eq!(std::fs::read(output).unwrap(), b"PREEXISTING CIA");
    }

    #[tokio::test]
    async fn decrypted_cdn_cancel_preserves_existing_output() {
        let tmp = tempfile::tempdir().unwrap();
        let cdn_dir = tmp.path().join("title");
        write_cdn_title(&cdn_dir, 0x0004000000030000);
        let output = tmp.path().join("title.cia");
        std::fs::write(&output, b"PREEXISTING CIA").unwrap();
        let mut opts = single_opts(cdn_dir, output.clone());
        opts.decrypt = true;
        opts.on_conflict = ConflictPolicy::Overwrite;
        let cancel = CancelToken::new();
        cancel.cancel();

        convert_cdn_to_cia_cancellable(opts, &NoProgress, &NoProgress, cancel)
            .await
            .expect_err("a pre-cancelled conversion must abort");

        assert_eq!(std::fs::read(output).unwrap(), b"PREEXISTING CIA");
    }

    #[test]
    fn final_publish_only_clobbers_for_overwrite_policy() {
        let tmp = tempfile::tempdir().unwrap();
        for policy in [
            ConflictPolicy::Error,
            ConflictPolicy::Skip,
            ConflictPolicy::Rename,
            ConflictPolicy::OverwriteInvalid,
        ] {
            let output = tmp.path().join(format!("{policy:?}.cia"));
            let staged = private_temp_path(&output, ".cia").unwrap();
            std::fs::write(&staged, b"NEW").unwrap();
            std::fs::write(&output, b"RACER").unwrap();

            let err = publish_temp_path(staged, &output, policy).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
            assert_eq!(std::fs::read(output).unwrap(), b"RACER");
        }

        let output = tmp.path().join("overwrite.cia");
        let staged = private_temp_path(&output, ".cia").unwrap();
        std::fs::write(&staged, b"NEW").unwrap();
        std::fs::write(&output, b"OLD").unwrap();
        publish_temp_path(staged, &output, ConflictPolicy::Overwrite).unwrap();
        assert_eq!(std::fs::read(output).unwrap(), b"NEW");
    }

    #[tokio::test]
    async fn cleanup_rejects_nested_output_before_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let cdn_dir = tmp.path().join("title");
        write_cdn_title(&cdn_dir, 0x0004000000030000);
        let output = cdn_dir.join("nested").join("out.cia");
        let mut opts = single_opts(cdn_dir.clone(), output.clone());
        opts.cleanup = true;

        let err = convert_cdn_to_cia(opts, &NoProgress, &NoProgress)
            .await
            .expect_err("cleanup must reject an output inside the source");

        assert!(err.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|err| err.kind() == std::io::ErrorKind::InvalidInput)
        }));
        assert!(cdn_dir.exists());
        assert!(!output.exists());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_containment_does_not_follow_final_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let cdn_dir = tmp.path().join("title");
        let outside = tmp.path().join("outside.cia");
        std::fs::create_dir(&cdn_dir).unwrap();
        std::fs::write(&outside, b"OUTSIDE").unwrap();
        let output = cdn_dir.join("output.cia");
        std::os::unix::fs::symlink(&outside, &output).unwrap();

        assert!(path_is_within(&output, &cdn_dir).unwrap());
    }

    fn is_ctr_cancelled(err: &anyhow::Error) -> bool {
        err.chain().any(|c| {
            matches!(
                c.downcast_ref::<NintendoCTRError>(),
                Some(NintendoCTRError::Cancelled)
            )
        })
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
        assert!(
            is_ctr_cancelled(&err),
            "error chain must carry the cancelled variant"
        );
        assert!(!output.exists(), "no partial output");
        assert!(!crate::util::scratch_output_exists(&output).unwrap());
        assert!(
            !ncch_scratch_present(tmp.path()),
            "no leftover .ncch scratch"
        );
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
        assert!(
            !ncch_scratch_present(dir.path()),
            "no .ncch scratch left behind"
        );

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
        assert!(!crate::util::scratch_output_exists(&output).unwrap());
        assert!(
            !ncch_scratch_present(tmp.path()),
            "no leftover .ncch scratch"
        );
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
        assert!(!crate::util::scratch_output_exists(&output).unwrap());
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
        let result =
            decrypt_rom_batch_cancellable(dir.path(), None, &NoProgress, &NoProgress, None, token)
                .await;

        let err = result.expect_err("a pre-cancelled token must abort the batch");
        assert!(
            is_ctr_cancelled(&err),
            "error chain must carry the cancelled variant"
        );
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
            .filter(|e| e.file_name().to_string_lossy().ends_with(".decrypted.cia"))
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
