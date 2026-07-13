use aes::{
    Aes128,
    cipher::{BlockModeEncrypt, KeyIvInit, StreamCipher},
};
use anyhow::{Context, Result, anyhow};
use binrw::{BinRead, BinWrite};
use byteorder::{BigEndian, ByteOrder, LittleEndian};
use log::{debug, info, warn};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter};

use crate::nintendo::ctr::constants::{
    CTR_KEYS_0, CTR_MEDIA_UNIT_SIZE, CTR_NCSD_PARTITIONS, EXEFS_ENTRY_SIZE, EXEFS_HEADER_SIZE,
    EXEFS_MAX_FILE_ENTRIES, EXEFS_SECTION_BANNER, EXEFS_SECTION_ICON,
    NCCH_FLAGS_EXTRA_CRYPTO_INDEX, NCCH_FLAGS_OFFSET, NCCH_FLAGS7_FIXED_KEY, NCCH_FLAGS7_NOCRYPTO,
    NCCH_FLAGS7_SEED_CRYPTO, NCCH_MAGIC_OFFSET, NCSD_PARTITION_COUNT, NCSD_PARTITION_ENTRY_SIZE,
    NCSD_PARTITION_TABLE_OFFSET, NCSD_TITLE_ID_OFFSET,
};
use crate::nintendo::ctr::decrypt::cia::{
    Aes128Ctr, ROMFS_CHUNK_SIZE, derive_ctr_key, extra_crypto_index, fixed_key,
    get_ncch_aes_counter, get_new_key,
};
use crate::nintendo::ctr::decrypt::model::NcchSection;
use crate::nintendo::ctr::decrypt::util::{derive_title_key_from_ticket, gen_iv};
use crate::nintendo::ctr::error::NintendoCTRError;
use crate::nintendo::ctr::models::cia::{CIA_HEADER_SIZE, CiaFile, CiaFileWithoutContent};
use crate::nintendo::ctr::models::exe_fs_header::ExeFSHeader;
use crate::nintendo::ctr::models::ncch_header::NcchHeader;
use crate::nintendo::ctr::models::title_metadata::{ContentInfoRecord, TitleMetadata};
use crate::nintendo::ctr::util::align_64;
use crate::nintendo::ctr::z3ds::models::underlying_magic;
use crate::util::{CancelToken, ProgressReporter, scratch_output_path};

const ENCRYPT_EXTS: &[&str] = &["cia", "3ds", "cci", "cxi"];
const COPY_BUF: usize = 4 * 1024 * 1024;
const CRYPTO_BUF: usize = 4 * 1024 * 1024;

pub fn derive_encrypted_path(input: &Path) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    let ext = input.extension().and_then(|s| s.to_str()).unwrap_or("");
    let name = if ext.is_empty() {
        format!("{stem}.encrypted")
    } else {
        format!("{stem}.encrypted.{ext}")
    };
    input.with_file_name(name)
}

pub async fn encrypt_rom(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
) -> Result<()> {
    encrypt_rom_cancellable(input, output, progress, CancelToken::new()).await
}

pub async fn encrypt_rom_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: CancelToken,
) -> Result<()> {
    let mut file = File::open(input).await?;

    let mut magic_buf = [0u8; 4];
    file.seek(SeekFrom::Start(NCCH_MAGIC_OFFSET as u64)).await?;
    file.read_exact(&mut magic_buf).await?;
    drop(file);

    if magic_buf == underlying_magic::NCSD {
        info!("Detected NCSD format (.3ds/.cci)");
        encrypt_ncsd_cancellable(input, output, progress, &cancel).await
    } else if magic_buf == underlying_magic::NCCH {
        info!("Detected standalone NCCH format (.cxi)");
        encrypt_ncch_cancellable(input, output, progress, &cancel).await
    } else {
        let mut file = File::open(input).await?;
        let mut header_check = [0u8; 4];
        file.read_exact(&mut header_check).await?;
        drop(file);

        if u32::from_le_bytes(header_check) == CIA_HEADER_SIZE {
            info!("Detected CIA format");
            encrypt_cia_cancellable(input, output, progress, &cancel).await
        } else {
            Err(anyhow!(
                "unrecognized format: no NCSD/NCCH magic at 0x100 and not a CIA file"
            ))
        }
    }
}

async fn encrypt_ncsd_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<()> {
    let input_size = fs::metadata(input).await?.len();
    progress.start(input_size, "Encrypting NCSD");

    let tmp = scratch_output_path(output)?;
    let result = async {
        fs::copy(input, &tmp).await?;
        encrypt_ncsd_partitions(input, &tmp, progress, cancel).await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Err(err) = result {
        fs::remove_file(&tmp).await.ok();
        return Err(err);
    }

    crate::util::publish_temp(tmp, output, true)?;
    progress.finish();
    info!("Encrypted NCSD file");
    Ok(())
}

async fn encrypt_ncch_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<()> {
    let input_size = fs::metadata(input).await?.len();
    progress.start(input_size, "Encrypting NCCH");

    let tmp = scratch_output_path(output)?;
    let result = async {
        fs::copy(input, &tmp).await?;
        encrypt_ncch_at(
            input,
            &tmp,
            0,
            [0u8; 8],
            NcchSource::Standalone,
            progress,
            cancel,
        )
        .await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Err(err) = result {
        fs::remove_file(&tmp).await.ok();
        return Err(err);
    }

    crate::util::publish_temp(tmp, output, true)?;
    progress.finish();
    info!("Encrypted NCCH file");
    Ok(())
}

pub async fn encrypt_rom_batch_cancellable(
    input_dir: &Path,
    output_dir: Option<&Path>,
    progress: &dyn ProgressReporter,
    total_progress: &dyn ProgressReporter,
    max_depth: Option<usize>,
    cancel: CancelToken,
) -> Result<()> {
    let roms = crate::util::fs::collect_files_with_exts(input_dir, ENCRYPT_EXTS, max_depth)?;
    if roms.is_empty() {
        warn!(
            "No supported ROM files found in {} (looked for {:?})",
            input_dir.display(),
            ENCRYPT_EXTS
        );
        return Ok(());
    }

    total_progress.start(
        roms.len() as u64,
        &format!("Encrypting {} files", roms.len()),
    );

    if let Some(dir) = output_dir {
        fs::create_dir_all(dir).await?;
    }

    for path in roms {
        if cancel.is_cancelled() {
            return Err(NintendoCTRError::Cancelled.into());
        }

        let output = crate::util::place_in_dir_mirrored(
            &derive_encrypted_path(&path),
            input_dir,
            output_dir,
        );
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).await?;
        }

        debug!("Encrypting {} -> {}", path.display(), output.display());
        if let Err(err) = encrypt_rom_cancellable(&path, &output, progress, cancel.clone()).await {
            if matches!(
                err.downcast_ref::<NintendoCTRError>(),
                Some(NintendoCTRError::Cancelled)
            ) {
                return Err(err);
            }
            warn!("Failed to encrypt {}: {err}", path.display());
        }

        total_progress.inc(1);
    }

    total_progress.finish();
    Ok(())
}

async fn encrypt_ncsd_partitions(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<()> {
    let mut rom_file = File::open(input).await?;

    let mut magic_buf = [0u8; 4];
    rom_file
        .seek(SeekFrom::Start(NCCH_MAGIC_OFFSET as u64))
        .await?;
    rom_file.read_exact(&mut magic_buf).await?;
    if magic_buf != underlying_magic::NCSD {
        anyhow::bail!("not a valid NCSD file: wrong magic");
    }

    let mut title_id = [0u8; 8];
    rom_file.seek(SeekFrom::Start(NCSD_TITLE_ID_OFFSET)).await?;
    rom_file.read_exact(&mut title_id).await?;

    rom_file
        .seek(SeekFrom::Start(NCSD_PARTITION_TABLE_OFFSET as u64))
        .await?;
    let mut table_buf = [0u8; NCSD_PARTITION_COUNT * NCSD_PARTITION_ENTRY_SIZE];
    rom_file.read_exact(&mut table_buf).await?;

    for (i, partition_name) in CTR_NCSD_PARTITIONS.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(NintendoCTRError::Cancelled.into());
        }

        let entry_offset = i * NCSD_PARTITION_ENTRY_SIZE;
        let offset_mu = u32::from_le_bytes(table_buf[entry_offset..entry_offset + 4].try_into()?);
        let size_mu = u32::from_le_bytes(table_buf[entry_offset + 4..entry_offset + 8].try_into()?);

        if offset_mu == 0 && size_mu == 0 {
            continue;
        }

        let partition_offset = offset_mu as u64 * CTR_MEDIA_UNIT_SIZE as u64;
        debug!(
            "  Partition {i} ({partition_name}) at offset 0x{partition_offset:X}, size {size_mu} MU",
        );

        encrypt_ncch_at(
            input,
            output,
            partition_offset,
            title_id,
            NcchSource::Ncsd,
            progress,
            cancel,
        )
        .await?;
    }

    Ok(())
}

async fn encrypt_cia_cancellable(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<()> {
    let input_size = fs::metadata(input).await?.len();
    progress.start(input_size, "Encrypting CIA");

    let tmp = scratch_output_path(output)?;
    let content_tmp = scratch_output_path(output)?;
    let result = async {
        let mut std_in = std::fs::File::open(input)?;
        let mut header_buf = [0u8; CIA_HEADER_SIZE as usize];
        std_in.read_exact(&mut header_buf)?;
        let header =
            crate::nintendo::ctr::models::cia::CiaHeader::read_le(&mut Cursor::new(header_buf))?;
        std_in.seek(SeekFrom::Start(0))?;
        let cia = CiaFileWithoutContent::read_le(&mut std_in)?;
        let layout = CiaLayout::new(&header);
        let title_key = derive_title_key_from_ticket(&mut std_in, layout.ticket_offset)?;
        let ticket_title_id = cia.ticket.ticket_data.title_id.to_be_bytes();

        let mut encrypted_cia = CiaFile {
            header: cia.header,
            cert_chain: cia.cert_chain,
            ticket: cia.ticket,
            tmd: cia.tmd,
            content_data: vec![],
            meta_data: None,
        };
        for record in &mut encrypted_cia.tmd.content_chunk_records {
            record.content_type.set_encrypted(true);
        }

        let mut preamble = Cursor::new(Vec::new());
        encrypted_cia.write_le(&mut preamble)?;
        let preamble_len = preamble.get_ref().len() as u64;

        let mut out = BufWriter::new(File::create(&tmp).await?);
        out.write_all(preamble.get_ref()).await?;
        out.flush().await?;

        let mut content_hashes = Vec::with_capacity(encrypted_cia.tmd.content_chunk_records.len());
        let mut next_content_offs = 0u64;
        for record in &encrypted_cia.tmd.content_chunk_records {
            if cancel.is_cancelled() {
                return Err(anyhow::Error::from(NintendoCTRError::Cancelled));
            }

            let content_offset = layout.content_offset + next_content_offs;
            copy_range_to_path(
                input,
                &content_tmp,
                content_offset,
                record.content_size,
                cancel,
            )
            .await?;

            encrypt_ncch_at(
                &content_tmp,
                &content_tmp,
                0,
                ticket_title_id,
                NcchSource::CiaContent {
                    content_index: record.content_index,
                },
                progress,
                cancel,
            )
            .await?;

            let hash = write_cbc_encrypted_content(
                &content_tmp,
                out.get_mut(),
                &title_key,
                record.content_index,
                progress,
                cancel,
            )
            .await?;
            content_hashes.push(hash);

            fs::remove_file(&content_tmp).await.ok();
            next_content_offs += align_64(record.content_size);
        }

        progress.finish();
        update_tmd_hashes(&mut encrypted_cia.tmd, &content_hashes)?;

        let mut finalized = Cursor::new(Vec::new());
        encrypted_cia.write_le(&mut finalized)?;
        if finalized.get_ref().len() as u64 != preamble_len {
            anyhow::bail!("CIA preamble length changed after hash fixup");
        }

        let end_pos = out.get_mut().stream_position().await?;
        out.get_mut().seek(SeekFrom::Start(0)).await?;
        out.get_mut().write_all(finalized.get_ref()).await?;
        out.get_mut().seek(SeekFrom::Start(end_pos)).await?;

        if header.meta_size > 0 {
            let aligned = align_64(end_pos);
            if aligned > end_pos {
                out.write_all(&vec![0u8; (aligned - end_pos) as usize])
                    .await?;
            }
            copy_tail_meta(input, out.get_mut(), header.meta_size as u64, cancel).await?;
        }

        out.flush().await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    fs::remove_file(&content_tmp).await.ok();
    if let Err(err) = result {
        fs::remove_file(&tmp).await.ok();
        return Err(err);
    }

    crate::util::publish_temp(tmp, output, true)?;
    info!("Encrypted CIA file");
    Ok(())
}

#[derive(Clone, Copy)]
enum NcchSource {
    Standalone,
    Ncsd,
    CiaContent { content_index: u16 },
}

impl NcchSource {
    fn cia_content_index(self) -> Option<u16> {
        match self {
            Self::CiaContent { content_index } => Some(content_index),
            _ => None,
        }
    }
}

async fn encrypt_ncch_at(
    input: &Path,
    output: &Path,
    ncch_offset: u64,
    mut title_id: [u8; 8],
    source: NcchSource,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<()> {
    let mut read = File::open(input).await?;
    let mut write = OpenOptions::new()
        .read(true)
        .write(true)
        .open(output)
        .await?;

    read.seek(SeekFrom::Start(ncch_offset)).await?;
    let mut header_bytes = [0u8; 0x200];
    read.read_exact(&mut header_bytes).await?;
    let header = NcchHeader::read(&mut Cursor::new(&header_bytes))?;
    if &header.magic != b"NCCH" {
        anyhow::bail!("not a valid NCCH partition at 0x{ncch_offset:X}");
    }

    if header.is_encrypted() {
        anyhow::bail!("input NCCH at 0x{ncch_offset:X} already appears encrypted");
    }

    if title_id.iter().all(|&x| x == 0) {
        title_id = header.programid;
        title_id.reverse();
    }

    let crypto = NcchCrypto::from_header(&header, title_id).await?;

    header_bytes[NCCH_FLAGS_OFFSET + 7] &= !NCCH_FLAGS7_NOCRYPTO;
    write.seek(SeekFrom::Start(ncch_offset)).await?;
    write.write_all(&header_bytes).await?;

    if header.exhdrsize != 0 {
        let counter = get_ncch_aes_counter(&header, NcchSection::ExHeader);
        let key = crypto.base_or_fixed_key();
        encrypt_stream_section(
            &mut read,
            &mut write,
            ncch_offset + EXEFS_HEADER_SIZE as u64,
            header.exhdrsize as u64 * 2,
            key,
            counter,
            None,
            progress,
            cancel,
        )
        .await?;
    }

    if header.exefssize != 0 {
        let counter = get_ncch_aes_counter(&header, NcchSection::ExeFS);
        encrypt_exefs_section(
            &mut read,
            &mut write,
            ncch_offset + (header.exefsoffset as u64 * CTR_MEDIA_UNIT_SIZE as u64),
            header.exefssize as u64 * CTR_MEDIA_UNIT_SIZE as u64,
            &crypto,
            counter,
            progress,
        )
        .await?;
    }

    if header.romfssize != 0 {
        let counter = get_ncch_aes_counter(&header, NcchSection::RomFS);
        let key = crypto.romfs_key();
        let cia_fixup = source
            .cia_content_index()
            .filter(|idx| *idx > 0)
            .map(|idx| idx as u8);
        encrypt_stream_section(
            &mut read,
            &mut write,
            ncch_offset + (header.romfsoffset as u64 * CTR_MEDIA_UNIT_SIZE as u64),
            header.romfssize as u64 * CTR_MEDIA_UNIT_SIZE as u64,
            key,
            counter,
            cia_fixup,
            progress,
            cancel,
        )
        .await?;
    }

    write.flush().await?;
    Ok(())
}

struct NcchCrypto {
    uses_extra_crypto: u8,
    use_seed_crypto: bool,
    fixed_crypto: u8,
    ncch_key_y: u128,
    key_y: u128,
}

impl NcchCrypto {
    async fn from_header(header: &NcchHeader, title_id: [u8; 8]) -> Result<Self> {
        let ncch_key_y = BigEndian::read_u128(header.signature[0..16].try_into()?);
        let uses_extra_crypto = header.flags[NCCH_FLAGS_EXTRA_CRYPTO_INDEX];

        let mut fixed_crypto = 0;
        if (header.flags[7] & NCCH_FLAGS7_FIXED_KEY) != 0 {
            let mut tid = header.titleid;
            tid.reverse();
            fixed_crypto = if (tid[3] & 16) != 0 { 2 } else { 1 };
        }

        let use_seed_crypto = (header.flags[7] & NCCH_FLAGS7_SEED_CRYPTO) != 0;
        let mut key_y = ncch_key_y;
        if use_seed_crypto {
            key_y = get_new_key(ncch_key_y, header, hex::encode(title_id)).await?;
        }

        Ok(Self {
            uses_extra_crypto,
            use_seed_crypto,
            fixed_crypto,
            ncch_key_y,
            key_y,
        })
    }

    fn base_or_fixed_key(&self) -> [u8; 16] {
        fixed_key(self.fixed_crypto)
            .unwrap_or_else(|| derive_ctr_key(CTR_KEYS_0[0], self.ncch_key_y))
    }

    fn extra_key(&self) -> [u8; 16] {
        derive_ctr_key(
            CTR_KEYS_0[extra_crypto_index(self.uses_extra_crypto)],
            self.key_y,
        )
    }

    fn romfs_key(&self) -> [u8; 16] {
        fixed_key(self.fixed_crypto).unwrap_or_else(|| self.extra_key())
    }
}

#[allow(clippy::too_many_arguments)]
async fn encrypt_stream_section(
    read: &mut File,
    write: &mut File,
    offset: u64,
    size: u64,
    key: [u8; 16],
    counter: [u8; 16],
    cia_cidx_fixup: Option<u8>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<()> {
    read.seek(SeekFrom::Start(offset)).await?;
    write.seek(SeekFrom::Start(offset)).await?;

    let mut remaining = size;
    let mut done = 0u64;
    let mut buf = vec![0u8; CRYPTO_BUF];
    while remaining > 0 {
        if cancel.is_cancelled() {
            return Err(NintendoCTRError::Cancelled.into());
        }

        let take = remaining.min(CRYPTO_BUF as u64) as usize;
        read.read_exact(&mut buf[..take]).await?;
        let chunk = &mut buf[..take];
        Aes128Ctr::new_from_slices(&key, &advance_counter(&counter, done))?.apply_keystream(chunk);
        if let Some(cidx) = cia_cidx_fixup {
            apply_cia_romfs_cidx_fixup(chunk, done, cidx);
        }
        write.write_all(chunk).await?;
        progress.inc(take as u64);
        remaining -= take as u64;
        done += take as u64;
    }

    Ok(())
}

fn apply_cia_romfs_cidx_fixup(chunk: &mut [u8], chunk_start: u64, cidx: u8) {
    let stride = ROMFS_CHUNK_SIZE as u64;
    let chunk_end = chunk_start + chunk.len() as u64;
    let mut fixup_pos = 1u64;
    if fixup_pos < chunk_start {
        fixup_pos += (chunk_start - fixup_pos).div_ceil(stride) * stride;
    }

    while fixup_pos < chunk_end {
        chunk[(fixup_pos - chunk_start) as usize] ^= cidx;
        fixup_pos += stride;
    }
}

async fn encrypt_exefs_section(
    read: &mut File,
    write: &mut File,
    offset: u64,
    size: u64,
    crypto: &NcchCrypto,
    counter: [u8; 16],
    progress: &dyn ProgressReporter,
) -> Result<()> {
    let mut plain = vec![0u8; size as usize];
    read.seek(SeekFrom::Start(offset)).await?;
    read.read_exact(&mut plain).await.context("reading ExeFS")?;

    let mut encrypted = plain.clone();
    Aes128Ctr::new_from_slices(&crypto.base_or_fixed_key(), &counter)?
        .apply_keystream(&mut encrypted);

    if crypto.uses_extra_crypto != 0 || crypto.use_seed_crypto {
        let mut extra_encrypted = plain.clone();
        Aes128Ctr::new_from_slices(&crypto.extra_key(), &counter)?
            .apply_keystream(&mut extra_encrypted);

        for entry_idx in 0usize..EXEFS_MAX_FILE_ENTRIES {
            let entry_start = entry_idx * EXEFS_ENTRY_SIZE;
            let entry_end = entry_start + EXEFS_ENTRY_SIZE;
            if entry_end > plain.len() {
                break;
            }
            let exe_info = ExeFSHeader::read(&mut Cursor::new(&plain[entry_start..entry_end]))?;
            let file_offset =
                LittleEndian::read_u32(&exe_info.file_offset) as usize + EXEFS_HEADER_SIZE;
            let file_size = LittleEndian::read_u32(&exe_info.file_size) as usize;
            if file_size == 0 || file_offset + file_size > plain.len() {
                continue;
            }

            let use_extra = match exe_info.file_name.iter().rposition(|&x| x != 0) {
                Some(name_end) if exe_info.file_name[..=name_end].is_ascii() => {
                    exe_info.file_name[..=name_end] != EXEFS_SECTION_ICON
                        && exe_info.file_name[..=name_end] != EXEFS_SECTION_BANNER
                }
                _ => true,
            };

            if use_extra {
                encrypted[file_offset..file_offset + file_size]
                    .copy_from_slice(&extra_encrypted[file_offset..file_offset + file_size]);
            }
        }
    }

    write.seek(SeekFrom::Start(offset)).await?;
    write.write_all(&encrypted).await.context("writing ExeFS")?;
    progress.inc(size);
    Ok(())
}

fn advance_counter(base: &[u8; 16], byte_offset: u64) -> [u8; 16] {
    let blocks = (byte_offset / 16) as u128;
    u128::from_be_bytes(*base)
        .wrapping_add(blocks)
        .to_be_bytes()
}

async fn copy_range_to_path(
    input: &Path,
    output: &Path,
    offset: u64,
    size: u64,
    cancel: &CancelToken,
) -> Result<()> {
    let mut src = File::open(input).await?;
    let mut dst = File::create(output).await?;
    src.seek(SeekFrom::Start(offset)).await?;
    copy_exact(&mut src, &mut dst, size, cancel).await?;
    dst.flush().await?;
    Ok(())
}

async fn copy_tail_meta(
    input: &Path,
    output: &mut File,
    meta_size: u64,
    cancel: &CancelToken,
) -> Result<()> {
    let input_len = fs::metadata(input).await?.len();
    if input_len < meta_size {
        anyhow::bail!("CIA header declares meta_size larger than input file");
    }
    let mut src = File::open(input).await?;
    src.seek(SeekFrom::Start(input_len - meta_size)).await?;
    copy_exact(&mut src, output, meta_size, cancel).await
}

async fn copy_exact(src: &mut File, dst: &mut File, size: u64, cancel: &CancelToken) -> Result<()> {
    let mut remaining = size;
    let mut buf = vec![0u8; COPY_BUF];
    while remaining > 0 {
        if cancel.is_cancelled() {
            return Err(NintendoCTRError::Cancelled.into());
        }
        let take = remaining.min(COPY_BUF as u64) as usize;
        src.read_exact(&mut buf[..take]).await?;
        dst.write_all(&buf[..take]).await?;
        remaining -= take as u64;
    }
    Ok(())
}

async fn write_cbc_encrypted_content(
    input: &Path,
    output: &mut File,
    title_key: &[u8; 16],
    content_index: u16,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> Result<[u8; 32]> {
    let mut src = File::open(input).await?;
    let size = src.metadata().await?.len();
    if size % 16 != 0 {
        anyhow::bail!("CIA content size is not AES-CBC block aligned: {size}");
    }

    let mut cipher = cbc::Encryptor::<Aes128>::new_from_slices(title_key, &gen_iv(content_index))?;
    let mut hasher = Sha256::new();
    let mut remaining = size;
    let mut buf = vec![0u8; COPY_BUF];
    while remaining > 0 {
        if cancel.is_cancelled() {
            return Err(NintendoCTRError::Cancelled.into());
        }

        let take = remaining.min(COPY_BUF as u64) as usize;
        src.read_exact(&mut buf[..take]).await?;
        for block in buf[..take].chunks_exact_mut(16) {
            let block = <&mut aes::cipher::array::Array<_, _>>::try_from(block)
                .map_err(|_| anyhow!("invalid AES block size"))?;
            cipher.encrypt_block(block);
        }
        hasher.update(&buf[..take]);
        output.write_all(&buf[..take]).await?;
        progress.inc(take as u64);
        remaining -= take as u64;
    }

    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok(out)
}

fn update_tmd_hashes(tmd: &mut TitleMetadata, content_hashes: &[[u8; 32]]) -> Result<()> {
    if content_hashes.len() != tmd.content_chunk_records.len() {
        anyhow::bail!(
            "encrypted {} contents but TMD declares {} records",
            content_hashes.len(),
            tmd.content_chunk_records.len()
        );
    }

    for (record, hash) in tmd.content_chunk_records.iter_mut().zip(content_hashes) {
        record.hash = hash.to_vec();
    }

    for content_info_record in &mut tmd.content_info_records {
        let start = content_info_record.content_index_offset as usize;
        let count = content_info_record.content_command_count as usize;
        let end = start + count;
        let mut hasher = Sha256::new();
        for chunk in &tmd.content_chunk_records[start..end] {
            let mut buf = Cursor::new(Vec::new());
            chunk.write_be(&mut buf)?;
            hasher.update(buf.get_ref());
        }
        content_info_record.hash = hasher.finalize().to_vec();
    }

    let mut hasher = Sha256::new();
    for content_info_record in &tmd.content_info_records {
        hash_content_info_record(&mut hasher, content_info_record)?;
    }
    tmd.header.content_info_records_hash = hasher.finalize().to_vec();
    Ok(())
}

fn hash_content_info_record(hasher: &mut Sha256, record: &ContentInfoRecord) -> Result<()> {
    let mut cursor = Cursor::new(Vec::new());
    record.content_index_offset.write_be(&mut cursor)?;
    record.content_command_count.write_be(&mut cursor)?;
    cursor.get_mut().extend_from_slice(&record.hash);
    hasher.update(cursor.get_ref());
    Ok(())
}

struct CiaLayout {
    ticket_offset: u64,
    content_offset: u64,
}

impl CiaLayout {
    fn new(header: &crate::nintendo::ctr::models::cia::CiaHeader) -> Self {
        let cert_offset = align_64(header.header_size as u64);
        let ticket_offset = align_64(cert_offset + header.cert_chain_size as u64);
        let tmd_offset = align_64(ticket_offset + header.ticket_size as u64);
        let content_offset = align_64(tmd_offset + header.tmd_size as u64);
        Self {
            ticket_offset,
            content_offset,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nintendo::ctr::constants::CIA_CONTENT_INDEX_SIZE;
    use crate::nintendo::ctr::decrypt::cia::parse_and_decrypt_ncch;
    use crate::nintendo::ctr::test_fixtures::{
        SYNTH_CIA_TITLE_ID, make_cert, make_ncch_header_bytes, make_ticket, make_tmd,
    };
    use crate::util::NoProgress;
    use binrw::{BinWrite, Endian};

    fn make_plain_ncch_with_romfs() -> Vec<u8> {
        let romfs_offset_mu = 2u32;
        let romfs_size_mu = 1u32;
        let mut data =
            vec![0u8; ((romfs_offset_mu + romfs_size_mu) * CTR_MEDIA_UNIT_SIZE) as usize];
        let header = make_ncch_header_bytes(SYNTH_CIA_TITLE_ID);
        data[..header.len()].copy_from_slice(&header);
        data[0x118..0x120].copy_from_slice(&SYNTH_CIA_TITLE_ID.to_le_bytes());
        data[0x1B0..0x1B4].copy_from_slice(&romfs_offset_mu.to_le_bytes());
        data[0x1B4..0x1B8].copy_from_slice(&romfs_size_mu.to_le_bytes());
        for (i, b) in data[(romfs_offset_mu as usize * CTR_MEDIA_UNIT_SIZE as usize)..]
            .iter_mut()
            .enumerate()
        {
            *b = (i as u8).wrapping_mul(17);
        }
        data
    }

    #[tokio::test]
    async fn ncch_encrypt_decrypt_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let plain_path = dir.path().join("plain.cxi");
        let encrypted_path = dir.path().join("encrypted.cxi");
        let decrypted_path = dir.path().join("decrypted.cxi");
        let plain = make_plain_ncch_with_romfs();
        std::fs::write(&plain_path, &plain).unwrap();

        encrypt_rom(&plain_path, &encrypted_path, &NoProgress)
            .await
            .unwrap();
        let mut out = File::create(&decrypted_path).await.unwrap();
        parse_and_decrypt_ncch(&encrypted_path, &mut out, &NoProgress, &CancelToken::new())
            .await
            .unwrap();
        out.flush().await.unwrap();

        assert_eq!(std::fs::read(&decrypted_path).unwrap(), plain);
    }

    #[tokio::test]
    async fn cia_encrypt_decrypt_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let plain_content = make_plain_ncch_with_romfs();
        let content_hash = {
            let mut h = Sha256::new();
            h.update(&plain_content);
            let d = h.finalize();
            let mut out = [0u8; 32];
            out.copy_from_slice(&d);
            out
        };

        let cert_chain = vec![
            make_cert(b"CA00000003", 0xAA),
            make_cert(b"CP0000000b", 0xBB),
            make_cert(b"XS0000000c", 0xCC),
        ];
        let ticket = make_ticket(SYNTH_CIA_TITLE_ID);
        let mut tmd = make_tmd(
            SYNTH_CIA_TITLE_ID,
            vec![(0, 0, plain_content.clone(), content_hash)],
        );
        update_tmd_hashes(&mut tmd, &[content_hash]).unwrap();

        let ticket_size = {
            let mut buf = Vec::new();
            ticket
                .write_options(&mut Cursor::new(&mut buf), Endian::Big, ())
                .unwrap();
            buf.len() as u32
        };
        let tmd_size = {
            let mut buf = Vec::new();
            tmd.write_options(&mut Cursor::new(&mut buf), Endian::Big, ())
                .unwrap();
            buf.len() as u32
        };

        let plain_cia = CiaFile {
            header: crate::nintendo::ctr::models::cia::CiaHeader {
                header_size: CIA_HEADER_SIZE,
                cia_type: 0,
                version: 0,
                cert_chain_size: 0x0A00,
                ticket_size,
                tmd_size,
                meta_size: 0,
                content_size: plain_content.len() as u64,
                content_index: vec![0x00; CIA_CONTENT_INDEX_SIZE],
            },
            cert_chain,
            ticket,
            tmd,
            content_data: plain_content,
            meta_data: None,
        };

        let plain_path = dir.path().join("plain.cia");
        let encrypted_path = dir.path().join("encrypted.cia");
        let decrypted_path = dir.path().join("decrypted.cia");
        let mut buf = Vec::new();
        plain_cia
            .write_options(&mut Cursor::new(&mut buf), Endian::Little, ())
            .unwrap();
        std::fs::write(&plain_path, &buf).unwrap();

        encrypt_rom(&plain_path, &encrypted_path, &NoProgress)
            .await
            .unwrap();
        crate::nintendo::ctr::decrypt_cia(&encrypted_path, &decrypted_path, &NoProgress)
            .await
            .unwrap();

        let decrypted_bytes = std::fs::read(&decrypted_path).unwrap();
        let decrypted_cia =
            CiaFile::read_options(&mut Cursor::new(&decrypted_bytes), Endian::Little, ()).unwrap();
        assert_bytes_eq(&decrypted_cia.content_data, &plain_cia.content_data);
        assert_bytes_eq(&decrypted_bytes, &buf);
    }

    #[tokio::test]
    async fn cia_romfs_cidx_fixup_matches_decrypt_chunk_cadence() {
        let dir = tempfile::tempdir().unwrap();
        let plain_path = dir.path().join("plain.romfs");
        let encrypted_path = dir.path().join("encrypted.romfs");
        let size = ROMFS_CHUNK_SIZE + 0x1000;
        let plain: Vec<u8> = (0..size)
            .map(|i| (i as u8).wrapping_mul(29).wrapping_add(7))
            .collect();
        std::fs::write(&plain_path, &plain).unwrap();
        std::fs::write(&encrypted_path, vec![0u8; size]).unwrap();

        let key = [0x42; 16];
        let counter = [0x11; 16];
        let cidx = 3;
        let mut read = File::open(&plain_path).await.unwrap();
        let mut write = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&encrypted_path)
            .await
            .unwrap();
        encrypt_stream_section(
            &mut read,
            &mut write,
            0,
            size as u64,
            key,
            counter,
            Some(cidx),
            &NoProgress,
            &CancelToken::new(),
        )
        .await
        .unwrap();
        write.flush().await.unwrap();

        let mut decrypted = std::fs::read(&encrypted_path).unwrap();
        let mut offset = 0usize;
        while offset < decrypted.len() {
            let end = (offset + ROMFS_CHUNK_SIZE).min(decrypted.len());
            let chunk = &mut decrypted[offset..end];
            if chunk.len() > 1 {
                chunk[1] ^= cidx;
            }
            Aes128Ctr::new_from_slices(&key, &advance_counter(&counter, offset as u64))
                .unwrap()
                .apply_keystream(chunk);
            offset = end;
        }

        assert_bytes_eq(&decrypted, &plain);
    }

    fn assert_bytes_eq(left: &[u8], right: &[u8]) {
        if left == right {
            return;
        }
        let pos = left
            .iter()
            .zip(right)
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| left.len().min(right.len()));
        panic!(
            "byte mismatch at {pos}: left={:?} right={:?}, len left={} right={}",
            left.get(pos),
            right.get(pos),
            left.len(),
            right.len()
        );
    }
}
