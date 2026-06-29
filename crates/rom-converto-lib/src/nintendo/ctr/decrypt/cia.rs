use aes::{
    Aes128,
    cipher::{KeyIvInit, StreamCipher},
};
use byteorder::{BigEndian, ByteOrder, LittleEndian};

use crate::nintendo::ctr::constants::{
    CTR_COMMON_KEYS_HEX, CTR_KEY_SCRAMBLE_C, CTR_KEYS_0, CTR_KEYS_1, CTR_MEDIA_UNIT_SIZE,
    CTR_NCSD_PARTITIONS, CTR_SEED_COUNTRIES, EXEFS_ENTRY_SIZE, EXEFS_HEADER_SIZE,
    EXEFS_MAX_FILE_ENTRIES, EXEFS_SECTION_BANNER, EXEFS_SECTION_ICON,
    NCCH_FLAGS_EXTRA_CRYPTO_INDEX, NCCH_FLAGS_OFFSET, NCCH_FLAGS7_CRYPTO_METHOD,
    NCCH_FLAGS7_FIXED_KEY, NCCH_FLAGS7_NOCRYPTO, NCCH_FLAGS7_SEED_CRYPTO, NCCH_MAGIC,
    NCCH_MAGIC_OFFSET, NCSD_PARTITION_COUNT, NCSD_PARTITION_ENTRY_SIZE,
    NCSD_PARTITION_TABLE_OFFSET, NCSD_TITLE_ID_OFFSET, TICKET_COMMON_KEY_IDX_OFFSET,
    TICKET_SIG_BODY_OFFSET, TICKET_TITLE_ID_OFFSET, TICKET_TITLE_KEY_OFFSET,
    TMD_CONTENT_COUNT_OFFSET, TMD_CONTENT_RECORD_SIZE, TMD_CONTENT_RECORDS_OFFSET,
};
use crate::nintendo::ctr::decrypt::model::{CiaContent, NcchSection};
use crate::nintendo::ctr::decrypt::reader::CiaReader;
use crate::nintendo::ctr::decrypt::romfs_worker::{
    RomfsChunk, RomfsChunkWork, RomfsDecryptWorker, advance_counter,
};
use crate::nintendo::ctr::decrypt::util::{cbc_decrypt, gen_iv};
use crate::nintendo::ctr::error::NintendoCTRError;
use crate::nintendo::ctr::models::cia::{CIA_HEADER_SIZE, CiaHeader};
use crate::nintendo::ctr::models::exe_fs_header::ExeFSHeader;
use crate::nintendo::ctr::models::ncch_header::NcchHeader;
use crate::nintendo::ctr::models::seeddb::SeedDatabase;
use crate::nintendo::ctr::models::title_metadata::ContentChunkRecord;
use crate::nintendo::ctr::util::align_64;
use crate::nintendo::ctr::z3ds::models::underlying_magic;
use crate::util::worker_pool::{Pool, parallelism};
use crate::util::{CancelToken, ProgressReporter};
use anyhow::{Context, anyhow};
use binrw::BinRead;
use futures::future::select_ok;
use lazy_static::lazy_static;
use log::debug;
use sha2::{Digest, Sha256};
use std::io::{Cursor, SeekFrom};
use std::{collections::HashMap, path::Path, vec};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter};

pub type Aes128Ctr = ctr::Ctr128BE<Aes128>;

// RomFS streams through the shared worker pool in fixed CHUNK_SIZE pieces.
// Peak working memory per RomFS region is ROMFS_MAX_IN_FLIGHT * CHUNK_SIZE
// for the in-flight queue plus the same again inside the worker threads
// (about 256 MiB), on top of one full ExeFS buffer (bounded at 8 MiB by the
// 3DS spec) and small fixed headers. The whole ROM/partition is never held
// in memory.
const CHUNK_SIZE: usize = 32 * 1024 * 1024; // 32 MiB
const ROMFS_MAX_IN_FLIGHT: usize = 4;

fn extra_crypto_index(uses_extra_crypto: u8) -> usize {
    match uses_extra_crypto {
        0 => 0,
        1 => 1,
        10 => 2,
        11 => 3,
        _ => 0,
    }
}

pub(crate) fn derive_ctr_key(key_x: u128, key_y: u128) -> [u8; 16] {
    u128::to_be_bytes(scramblekey(key_x, key_y))
}

fn fixed_key(fixed_crypto: u8) -> Option<[u8; 16]> {
    (fixed_crypto != 0).then(|| u128::to_be_bytes(CTR_KEYS_1[(fixed_crypto as usize) - 1]))
}

type ContentHasher<'a> = Option<&'a mut Sha256>;

fn hash_bytes(hasher: &mut ContentHasher<'_>, bytes: &[u8]) {
    if let Some(h) = hasher.as_deref_mut() {
        h.update(bytes);
    }
}

async fn advance_to_offset(
    writer: &mut BufWriter<&mut File>,
    cia: &mut CiaReader,
    out_base: u64,
    target_offset: u64,
    hasher: &mut ContentHasher<'_>,
) -> anyhow::Result<()> {
    if let Some(gap) = target_offset.checked_sub(writer.stream_position().await?)
        && gap > 0
    {
        let mut buf = vec![0u8; gap as usize];
        cia.read(&mut buf)
            .await
            .context("reading gap bytes before section")?;
        // At the NCCH header boundary (0x200 into the NCCH), clear the second
        // byte to fix the content-index field after decryption. out_base
        // shifts the comparison to the partition's absolute position when the
        // writer targets a multi-partition NCSD output.
        if writer.stream_position().await? == out_base + EXEFS_HEADER_SIZE as u64 {
            buf[1] = 0x00;
        }
        hash_bytes(hasher, &buf);
        writer
            .write_all(&buf)
            .await
            .context("writing gap bytes before section")?;
    }
    Ok(())
}

async fn copy_plain_section(
    cia: &mut CiaReader,
    writer: &mut BufWriter<&mut File>,
    size: u32,
    hasher: &mut ContentHasher<'_>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> anyhow::Result<()> {
    let mut remaining_bytes = size;
    let mut buf = vec![0u8; CHUNK_SIZE];

    while remaining_bytes > CHUNK_SIZE as u32 {
        if cancel.is_cancelled() {
            return Err(NintendoCTRError::Cancelled.into());
        }
        cia.read(&mut buf).await.context("reading plain chunk")?;
        hash_bytes(hasher, &buf);
        writer
            .write_all(&buf)
            .await
            .context("writing plain chunk")?;
        remaining_bytes -= CHUNK_SIZE as u32;
        progress.inc(CHUNK_SIZE as u64);
    }

    if remaining_bytes > 0 {
        let tail = &mut buf[..remaining_bytes as usize];
        cia.read(tail).await.context("reading final plain chunk")?;
        hash_bytes(hasher, tail);
        writer
            .write_all(tail)
            .await
            .context("writing final plain chunk")?;
        progress.inc(remaining_bytes as u64);
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn write_exheader_section(
    cia: &mut CiaReader,
    writer: &mut BufWriter<&mut File>,
    size: u32,
    ctr: &[u8; 16],
    base_key: [u8; 16],
    fixed_crypto: u8,
    hasher: &mut ContentHasher<'_>,
    progress: &dyn ProgressReporter,
) -> anyhow::Result<()> {
    let mut key = base_key;
    if let Some(fixed) = fixed_key(fixed_crypto) {
        key = fixed;
    }

    let mut buf = vec![0u8; size as usize];
    cia.read(&mut buf).await.context("reading ExHeader")?;
    Aes128Ctr::new_from_slices(&key, ctr)?.apply_keystream(&mut buf);
    hash_bytes(hasher, &buf);
    writer.write_all(&buf).await.context("writing ExHeader")?;
    progress.inc(size as u64);
    Ok(())
}

#[derive(Debug)]
struct ExefsDecryptOptions {
    size: u32,
    ctr: [u8; 16],
    base_key: [u8; 16],
    uses_extra_crypto: u8,
    fixed_crypto: u8,
    use_seed_crypto: bool,
    key_y: u128,
}

async fn write_exefs_section(
    cia: &mut CiaReader,
    writer: &mut BufWriter<&mut File>,
    opts: ExefsDecryptOptions,
    hasher: &mut ContentHasher<'_>,
    progress: &dyn ProgressReporter,
) -> anyhow::Result<()> {
    let mut working_key = opts.base_key;
    if let Some(fixed) = fixed_key(opts.fixed_crypto) {
        working_key = fixed;
    }

    let mut encrypted_exefs = vec![0u8; opts.size as usize];
    cia.read(&mut encrypted_exefs)
        .await
        .context("reading ExeFS")?;

    let mut decrypted_exefs = encrypted_exefs.clone();
    Aes128Ctr::new_from_slices(&working_key, &opts.ctr)?.apply_keystream(&mut decrypted_exefs);

    if opts.uses_extra_crypto != 0 || opts.use_seed_crypto {
        let mut extra_decrypted = encrypted_exefs;
        let extra_key = derive_ctr_key(
            CTR_KEYS_0[extra_crypto_index(opts.uses_extra_crypto)],
            opts.key_y,
        );
        Aes128Ctr::new_from_slices(&extra_key, &opts.ctr)?.apply_keystream(&mut extra_decrypted);

        for entry_idx in 0usize..EXEFS_MAX_FILE_ENTRIES {
            let entry_bytes =
                &decrypted_exefs[entry_idx * EXEFS_ENTRY_SIZE..(entry_idx + 1) * EXEFS_ENTRY_SIZE];
            let exe_info = ExeFSHeader::read(&mut Cursor::new(entry_bytes))?;

            let offset = LittleEndian::read_u32(&exe_info.file_offset) as usize + EXEFS_HEADER_SIZE;
            let size = LittleEndian::read_u32(&exe_info.file_size) as usize;

            match exe_info.file_name.iter().rposition(|&x| x != 0) {
                Some(name_end) if exe_info.file_name[..=name_end].is_ascii() => {
                    if exe_info.file_name[..=name_end] != EXEFS_SECTION_ICON
                        && exe_info.file_name[..=name_end] != EXEFS_SECTION_BANNER
                    {
                        decrypted_exefs[offset..offset + size]
                            .copy_from_slice(&extra_decrypted[offset..offset + size]);
                    }
                }
                _ => {
                    decrypted_exefs[offset..offset + size]
                        .copy_from_slice(&extra_decrypted[offset..offset + size]);
                }
            }
        }
    }

    hash_bytes(hasher, &decrypted_exefs);
    writer
        .write_all(&decrypted_exefs)
        .await
        .context("writing ExeFS")?;
    progress.inc(opts.size as u64);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn write_romfs_section(
    cia: &mut CiaReader,
    writer: &mut BufWriter<&mut File>,
    size: u32,
    ctr: &[u8; 16],
    uses_extra_crypto: u8,
    fixed_crypto: u8,
    key_y: u128,
    hasher: &mut ContentHasher<'_>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> anyhow::Result<()> {
    let mut key = derive_ctr_key(CTR_KEYS_0[extra_crypto_index(uses_extra_crypto)], key_y);
    if let Some(fixed) = fixed_key(fixed_crypto) {
        key = fixed;
    }

    let base_ctr = *ctr;
    // The producer reads through the CIA outer CBC layer (sequential), so
    // chunks must be read in order; only the per-chunk AES-CTR step runs in
    // parallel across the pool. The legacy path XORed cidx into byte 1 of every
    // chunk buffer, so the fixup is applied per chunk here to stay byte-identical.
    let apply_cidx_fixup = cia.cidx > 0 && !(cia.single_ncch || cia.from_ncsd);
    let cidx_byte = cia.cidx as u8;
    let total_size = size as u64;
    let n_chunks = total_size.div_ceil(CHUNK_SIZE as u64);

    let workers: Vec<RomfsDecryptWorker> = (0..parallelism()).map(|_| RomfsDecryptWorker).collect();
    let pool: Pool<RomfsChunkWork, RomfsChunk, NintendoCTRError> = Pool::spawn(workers);

    let mut pending: HashMap<u64, Vec<u8>> = HashMap::new();
    let mut submit_seq: u64 = 0;
    let mut write_seq: u64 = 0;
    let mut in_flight: usize = 0;
    let mut bytes_read: u64 = 0;

    let result = async {
        while write_seq < n_chunks {
            while in_flight < ROMFS_MAX_IN_FLIGHT && submit_seq < n_chunks {
                if cancel.is_cancelled() {
                    return Err(anyhow::Error::from(NintendoCTRError::Cancelled));
                }
                let this = std::cmp::min(CHUNK_SIZE as u64, total_size - bytes_read) as usize;
                let mut buf = vec![0u8; this];
                cia.read(&mut buf).await.context("reading RomFS chunk")?;
                if apply_cidx_fixup {
                    buf[1] ^= cidx_byte;
                }
                let counter = advance_counter(&base_ctr, bytes_read);
                pool.submit(
                    submit_seq,
                    RomfsChunkWork {
                        key,
                        counter,
                        data: buf,
                    },
                )
                .map_err(NintendoCTRError::from)?;
                bytes_read += this as u64;
                submit_seq += 1;
                in_flight += 1;
            }

            let (seq, res) = pool.recv();
            in_flight -= 1;
            pending.insert(seq, res?.data);

            while let Some(data) = pending.remove(&write_seq) {
                hash_bytes(hasher, &data);
                writer
                    .write_all(&data)
                    .await
                    .context("writing RomFS chunk")?;
                progress.inc(data.len() as u64);
                write_seq += 1;
            }
        }
        Ok(())
    }
    .await;

    pool.shutdown();
    result
}

pub(crate) fn get_ncch_aes_counter(hdr: &NcchHeader, section: NcchSection) -> [u8; 16] {
    let mut counter: [u8; 16] = [0; 16];
    if hdr.formatversion == 2 || hdr.formatversion == 0 {
        let mut titleid: [u8; 8] = hdr.titleid;
        titleid.reverse();
        counter[0..8].copy_from_slice(&titleid);
        counter[8] = section as u8;
    } else if hdr.formatversion == 1 {
        let x = match section {
            NcchSection::ExHeader => 512,
            NcchSection::ExeFS => hdr.exefsoffset * CTR_MEDIA_UNIT_SIZE,
            NcchSection::RomFS => hdr.romfsoffset * CTR_MEDIA_UNIT_SIZE,
        };

        counter[0..8].copy_from_slice(&hdr.titleid);
        for i in 0..4 {
            counter[12 + i] = (x >> ((3 - i) * 8) & 255) as u8
        }
    }

    counter
}

fn scramblekey(key_x: u128, key_y: u128) -> u128 {
    const MAX_BITS: u32 = 128;

    let rol = |val: u128, r_bits: u32| (val << r_bits) | (val >> (MAX_BITS - r_bits));

    let value = rol(key_x, 2) ^ key_y;
    rol(value.wrapping_add(CTR_KEY_SCRAMBLE_C), 87)
}

async fn fetch_seed(title_id: &str) -> anyhow::Result<[u8; 16]> {
    lazy_static! {
        // Nintendo's seed CDN serves a custom certificate that won't chain to the
        // standard root store, so disabling TLS validation is the simplest way to
        // reach it.
        static ref CLIENT: reqwest::Client = reqwest::Client::builder()
            .tls_danger_accept_invalid_certs(true)
            .build()
            .expect("Failed to create HTTP client");
    }

    // Build a future for each country, returning Ok(bytes) on 200 or Err otherwise
    let requests = CTR_SEED_COUNTRIES.iter().map(|&country| {
        let client = &*CLIENT;
        debug!("Fetching seed for {country} ({title_id})");
        let url = format!(
            "https://kagiya-ctr.cdn.nintendo.net/title/0x{title_id}/ext_key?country={country}"
        );
        Box::pin(async move {
            let resp = client.get(&url).send().await?;
            if resp.status().is_success() {
                let bytes = resp.bytes().await?;
                Ok(bytes)
            } else {
                Err(anyhow!("HTTP {} for {}", resp.status(), country))
            }
        })
    });

    // Run all requests in parallel and take the first successful one
    let (bytes, _others) = select_ok(requests).await?;

    let key: [u8; 16] = <[u8; 16]>::try_from(bytes.as_ref())
        .map_err(|e| anyhow!("Failed to parse key bytes: {}", e))?;

    Ok(key)
}

/// Parameters required to write a decrypted NCCH section.
#[derive(Debug)]
struct NcchWriteOptions {
    offset: u64,
    size: u32,
    section: NcchSection,
    counter: [u8; 16],
    uses_extra_crypto: u8,
    fixed_crypto: u8,
    use_seed_crypto: bool,
    encrypted: bool,
    keys: [u128; 2],
}

#[allow(clippy::too_many_arguments)]
async fn write_to_file(
    writer: &mut BufWriter<&mut File>,
    cia: &mut CiaReader,
    out_base: u64,
    opts: NcchWriteOptions,
    hasher: &mut ContentHasher<'_>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> anyhow::Result<()> {
    advance_to_offset(writer, cia, out_base, out_base + opts.offset, hasher).await?;

    if !opts.encrypted {
        copy_plain_section(cia, writer, opts.size, hasher, progress, cancel).await?;
        return Ok(());
    }

    let base_key = derive_ctr_key(CTR_KEYS_0[0], opts.keys[0]);

    match opts.section {
        NcchSection::ExHeader => {
            write_exheader_section(
                cia,
                writer,
                opts.size,
                &opts.counter,
                base_key,
                opts.fixed_crypto,
                hasher,
                progress,
            )
            .await?
        }
        NcchSection::ExeFS => {
            write_exefs_section(
                cia,
                writer,
                ExefsDecryptOptions {
                    size: opts.size,
                    ctr: opts.counter,
                    base_key,
                    uses_extra_crypto: opts.uses_extra_crypto,
                    fixed_crypto: opts.fixed_crypto,
                    use_seed_crypto: opts.use_seed_crypto,
                    key_y: opts.keys[1],
                },
                hasher,
                progress,
            )
            .await?
        }
        NcchSection::RomFS => {
            write_romfs_section(
                cia,
                writer,
                opts.size,
                &opts.counter,
                opts.uses_extra_crypto,
                opts.fixed_crypto,
                opts.keys[1],
                hasher,
                progress,
                cancel,
            )
            .await?
        }
    };

    Ok(())
}

async fn get_new_key(key_y: u128, header: &NcchHeader, title_id: String) -> anyhow::Result<u128> {
    lazy_static! {
        static ref SEEDS: HashMap<String, [u8; 16]> = {
            let db_path = Path::new("seeddb.bin");
            if let Ok(data) = std::fs::read(db_path)
                && let Ok(seeddb) = SeedDatabase::read(&mut Cursor::new(data))
            {
                debug!("Loading {} seeds from seeddb.bin", seeddb.seed_count);
                seeddb
                    .seeds
                    .into_iter()
                    .map(|seed| (seed.key, seed.value))
                    .collect()
            } else {
                debug!("No seeddb.bin found, starting with an empty seed map");
                HashMap::new()
            }
        };
    }

    let mut seed = SEEDS.get(&title_id).copied();

    if seed.is_none() {
        let api_seed = fetch_seed(&title_id).await?;
        seed = Some(api_seed)
    }

    if let Some(seed) = seed {
        let seed_check = BigEndian::read_u32(&header.seedcheck);
        let mut revtid = hex::decode(&title_id)?;
        revtid.reverse();
        let sha_sum = sha256::digest([seed.to_vec(), revtid].concat());

        if BigEndian::read_u32(&hex::decode(&sha_sum[..8])?) == seed_check {
            let keystr = sha256::digest([u128::to_be_bytes(key_y), seed].concat());
            return Ok(BigEndian::read_u128(&hex::decode(&keystr[..32])?));
        }
    }

    Err(anyhow!(
        "Seed verification failed: SHA256 mismatch for title {title_id}"
    ))
}

#[allow(clippy::too_many_arguments)]
pub async fn parse_ncch(
    cia: &mut CiaReader,
    out: &mut File,
    out_base: u64,
    offs: u64,
    mut title_id: [u8; 8],
    mut hasher: ContentHasher<'_>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> anyhow::Result<()> {
    if cia.from_ncsd {
        debug!("  Parsing {} NCCH", CTR_NCSD_PARTITIONS[cia.cidx as usize]);
    } else if cia.single_ncch {
        debug!(
            "  Parsing NCCH in file: {}",
            cia.path.file_name().and_then(|s| s.to_str()).unwrap_or("")
        );
    } else {
        debug!("Parsing NCCH: {}", cia.cidx)
    }

    cia.seek(offs).await?;
    let mut tmp = [0u8; 512];
    cia.read(&mut tmp).await?;
    let header = NcchHeader::read(&mut Cursor::new(&tmp))?;
    if title_id.iter().all(|&x| x == 0) {
        title_id = header.programid;
        title_id.reverse();
    }

    let ncch_key_y = BigEndian::read_u128(header.signature[0..16].try_into()?);
    let mut tid: [u8; 8] = header.titleid;
    tid.reverse();

    let uses_extra_crypto: u8 = header.flags[NCCH_FLAGS_EXTRA_CRYPTO_INDEX];

    if uses_extra_crypto != 0 {
        debug!("  Uses extra NCCH crypto, keyslot 0x25");
    }

    let mut fixed_crypto: u8 = 0;
    let mut encrypted: bool = true;

    if (header.flags[7] & NCCH_FLAGS7_FIXED_KEY) != 0 {
        if (tid[3] & 16) != 0 {
            fixed_crypto = 2
        } else {
            fixed_crypto = 1
        }
        debug!("  Uses fixed-key crypto")
    }

    if (header.flags[7] & NCCH_FLAGS7_NOCRYPTO) != 0 {
        encrypted = false;
        debug!("  Not encrypted")
    }

    let use_seed_crypto: bool = (header.flags[7] & NCCH_FLAGS7_SEED_CRYPTO) != 0;
    let mut key_y = ncch_key_y;

    if use_seed_crypto {
        key_y = get_new_key(ncch_key_y, &header, hex::encode(title_id)).await?;
        debug!("Uses 9.6 NCCH Seed crypto with KeyY: {key_y:032X}");
    }

    // Preserve the crypto-method bit, set the NoCrypto flag (content is now decrypted)
    tmp[NCCH_FLAGS_OFFSET + 7] =
        tmp[NCCH_FLAGS_OFFSET + 7] & NCCH_FLAGS7_CRYPTO_METHOD | NCCH_FLAGS7_NOCRYPTO;

    out.seek(SeekFrom::Start(out_base)).await?;
    let mut writer = BufWriter::new(out);

    hash_bytes(&mut hasher, &tmp);
    writer.write_all(&tmp).await?;

    let mut counter: [u8; 16];
    if header.exhdrsize != 0 {
        counter = get_ncch_aes_counter(&header, NcchSection::ExHeader);
        write_to_file(
            &mut writer,
            cia,
            out_base,
            NcchWriteOptions {
                offset: EXEFS_HEADER_SIZE as u64,
                size: header.exhdrsize * 2,
                section: NcchSection::ExHeader,
                counter,
                uses_extra_crypto,
                fixed_crypto,
                use_seed_crypto,
                encrypted,
                keys: [ncch_key_y, key_y],
            },
            &mut hasher,
            progress,
            cancel,
        )
        .await?;
    }

    if header.exefssize != 0 {
        counter = get_ncch_aes_counter(&header, NcchSection::ExeFS);
        write_to_file(
            &mut writer,
            cia,
            out_base,
            NcchWriteOptions {
                offset: (header.exefsoffset * CTR_MEDIA_UNIT_SIZE) as u64,
                size: header.exefssize * CTR_MEDIA_UNIT_SIZE,
                section: NcchSection::ExeFS,
                counter,
                uses_extra_crypto,
                fixed_crypto,
                use_seed_crypto,
                encrypted,
                keys: [ncch_key_y, key_y],
            },
            &mut hasher,
            progress,
            cancel,
        )
        .await?;
    }

    if header.romfssize != 0 {
        counter = get_ncch_aes_counter(&header, NcchSection::RomFS);
        write_to_file(
            &mut writer,
            cia,
            out_base,
            NcchWriteOptions {
                offset: (header.romfsoffset * CTR_MEDIA_UNIT_SIZE) as u64,
                size: header.romfssize * CTR_MEDIA_UNIT_SIZE,
                section: NcchSection::RomFS,
                counter,
                uses_extra_crypto,
                fixed_crypto,
                use_seed_crypto,
                encrypted,
                keys: [ncch_key_y, key_y],
            },
            &mut hasher,
            progress,
            cancel,
        )
        .await?;
    }

    writer.flush().await?;

    Ok(())
}

pub async fn parse_and_decrypt_ncsd(
    input: &Path,
    out: &mut File,
    partition: Option<u8>,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> anyhow::Result<()> {
    debug!("Parsing NCSD file: {}", input.display());

    let mut rom_file = File::open(input).await?;

    // Verify NCSD magic at offset 0x100
    let mut magic_buf = [0u8; 4];
    rom_file
        .seek(SeekFrom::Start(NCCH_MAGIC_OFFSET as u64))
        .await?;
    rom_file.read_exact(&mut magic_buf).await?;
    if magic_buf != underlying_magic::NCSD {
        return Err(anyhow!("Not a valid NCSD file: wrong magic"));
    }

    let mut title_id = [0u8; 8];
    rom_file.seek(SeekFrom::Start(NCSD_TITLE_ID_OFFSET)).await?;
    rom_file.read_exact(&mut title_id).await?;

    // Partition table: 8 entries, each (offset_mu: u32, size_mu: u32) LE.
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

        if let Some(target) = partition
            && i as u8 != target
        {
            continue;
        }

        let partition_offset = offset_mu as u64 * CTR_MEDIA_UNIT_SIZE as u64;

        debug!(
            "  Partition {i} ({partition_name}) at offset 0x{partition_offset:X}, size {size_mu} MU",
        );

        let mut reader = CiaReader::new(
            rom_file.try_clone().await?,
            false,
            input.to_path_buf(),
            [0u8; 16],
            i as u32,
            i as u16,
            0,
            false,
            true,
        );

        parse_ncch(
            &mut reader,
            out,
            partition_offset,
            partition_offset,
            title_id,
            None,
            progress,
            cancel,
        )
        .await?;
    }

    Ok(())
}

pub async fn parse_and_decrypt_ncch(
    input: &Path,
    out: &mut File,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> anyhow::Result<()> {
    debug!("Parsing standalone NCCH file: {}", input.display());

    let rom_file = File::open(input).await?;

    let mut reader = CiaReader::new(
        rom_file,
        false,
        input.to_path_buf(),
        [0u8; 16],
        0,
        0,
        0,
        true,
        false,
    );

    parse_ncch(&mut reader, out, 0, 0, [0u8; 8], None, progress, cancel).await?;

    Ok(())
}

/// Decrypts every NCCH content of a CIA and writes the decrypted bytes
/// directly into `out` at its current position, in TMD-record order. Returns
/// the SHA-256 of each decrypted content, indexed by record order, so the
/// caller can recompute the TMD content hashes without a read-back pass.
pub async fn parse_and_decrypt_cia(
    input: &Path,
    out: &mut File,
    progress: &dyn ProgressReporter,
    cancel: &CancelToken,
) -> anyhow::Result<Vec<[u8; 32]>> {
    debug!("Parsing CIA file: {}", input.display());

    let mut rom_file = File::open(input).await?;

    let mut header_buf = vec![0u8; CIA_HEADER_SIZE as usize];
    rom_file.read_exact(&mut header_buf).await?;
    let cia_header = CiaHeader::read_le(&mut Cursor::new(&header_buf))?;

    let cachainoff = align_64(cia_header.header_size as u64);
    let tikoff = align_64(cachainoff + cia_header.cert_chain_size as u64);
    let tmdoff = align_64(tikoff + cia_header.ticket_size as u64);
    let contentoffs = align_64(tmdoff + cia_header.tmd_size as u64);

    rom_file
        .seek(SeekFrom::Start(
            tikoff + TICKET_SIG_BODY_OFFSET + TICKET_TITLE_KEY_OFFSET,
        ))
        .await?;
    let mut enckey: [u8; 16] = [0; 16];
    rom_file.read_exact(&mut enckey).await?;
    rom_file
        .seek(SeekFrom::Start(
            tikoff + TICKET_SIG_BODY_OFFSET + TICKET_TITLE_ID_OFFSET,
        ))
        .await?;
    let mut tid: [u8; 16] = [0; 16];
    rom_file.read_exact(&mut tid[0..8]).await?;

    if hex::encode(tid).starts_with("00048") {
        return Err(anyhow::anyhow!("Unsupported CIA file"));
    }

    rom_file
        .seek(SeekFrom::Start(
            tikoff + TICKET_SIG_BODY_OFFSET + TICKET_COMMON_KEY_IDX_OFFSET,
        ))
        .await?;
    let mut cmnkeyidx: u8 = 0;
    rom_file
        .read_exact(std::slice::from_mut(&mut cmnkeyidx))
        .await?;

    cbc_decrypt(&CTR_COMMON_KEYS_HEX[cmnkeyidx as usize], &tid, &mut enckey)?;
    let title_key = enckey;

    rom_file
        .seek(SeekFrom::Start(tmdoff + TMD_CONTENT_COUNT_OFFSET))
        .await?;
    let mut content_count: [u8; 2] = [0; 2];
    rom_file.read_exact(&mut content_count).await?;

    let mut hashes: Vec<[u8; 32]> =
        Vec::with_capacity(BigEndian::read_u16(&content_count) as usize);
    let mut next_content_offs = 0;
    let mut out_pos = out.stream_position().await?;
    for i in 0..BigEndian::read_u16(&content_count) {
        if cancel.is_cancelled() {
            return Err(NintendoCTRError::Cancelled.into());
        }
        rom_file
            .seek(SeekFrom::Start(
                tmdoff + TMD_CONTENT_RECORDS_OFFSET + (TMD_CONTENT_RECORD_SIZE * i as u64),
            ))
            .await?;
        let mut record_buf = vec![0u8; TMD_CONTENT_RECORD_SIZE as usize];
        rom_file.read_exact(&mut record_buf).await?;
        let record = ContentChunkRecord::read_be(&mut Cursor::new(&record_buf))?;

        let content = CiaContent {
            cid: record.content_id,
            cidx: record.content_index,
            ctype: record.content_type.0,
            csize: record.content_size,
        };

        let cenc = (content.ctype & 1) != 0;

        rom_file
            .seek(SeekFrom::Start(contentoffs + next_content_offs))
            .await?;
        let mut probe_buf: [u8; 512] = [0; 512];
        rom_file.read_exact(&mut probe_buf).await?;
        let mut magic: [u8; 4] = probe_buf[256..260].try_into()?;

        let iv: [u8; 16] = gen_iv(content.cidx);

        if cenc {
            cbc_decrypt(&title_key, &iv, &mut probe_buf)?;
            magic = probe_buf[256..260].try_into()?;
        }

        match std::str::from_utf8(&magic) {
            Ok(utf8) => {
                if utf8 == NCCH_MAGIC {
                    rom_file
                        .seek(SeekFrom::Start(contentoffs + next_content_offs))
                        .await?;
                    let mut cia_handle = CiaReader::new(
                        rom_file.try_clone().await?,
                        cenc,
                        input.to_path_buf(),
                        title_key,
                        content.cid,
                        content.cidx,
                        contentoffs + next_content_offs,
                        false,
                        false,
                    );
                    next_content_offs += align_64(content.csize);

                    let mut hasher = Sha256::new();
                    parse_ncch(
                        &mut cia_handle,
                        out,
                        out_pos,
                        0,
                        tid[0..8].try_into()?,
                        Some(&mut hasher),
                        progress,
                        cancel,
                    )
                    .await?;
                    out_pos = out.stream_position().await?;
                    hashes.push(hasher.finalize().into());
                } else {
                    return Err(anyhow!("Cia can't be parsed"));
                }
            }
            Err(e) => return Err(anyhow!(e)),
        }
    }

    Ok(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;
    use tokio::io::AsyncReadExt;

    /// The pooled RomFS path must decrypt to the exact bytes a single
    /// continuous AES-CTR stream would produce, including the per-region cidx
    /// fixup on byte 1. Uses a standalone-NCCH reader (no CIA outer CBC) so the
    /// only crypto under test is the inner CTR streaming.
    #[tokio::test]
    async fn write_romfs_section_matches_continuous_stream() {
        let key_y: u128 = 0x0123_4567_89AB_CDEF_0011_2233_4455_6677;
        let counter: [u8; 16] = [
            0xAA, 0xBB, 0xCC, 0xDD, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ];
        let size: u32 = 0x4000;

        let plaintext: Vec<u8> = (0..size)
            .map(|i| (i.wrapping_mul(31) % 251) as u8)
            .collect();
        let key = derive_ctr_key(CTR_KEYS_0[0], key_y);

        // Reference: encrypt the plaintext with one continuous keystream; the
        // decrypt must invert it back to the plaintext.
        let mut encrypted = plaintext.clone();
        Aes128Ctr::new_from_slices(&key, &counter)
            .unwrap()
            .apply_keystream(&mut encrypted);

        let tmp = tempfile::tempdir().unwrap();
        let in_path = tmp.path().join("romfs.bin");
        std::fs::write(&in_path, &encrypted).unwrap();
        let out_path = tmp.path().join("out.bin");

        let in_file = File::open(&in_path).await.unwrap();
        let mut reader = CiaReader::new(
            in_file,
            false,
            in_path.clone(),
            [0u8; 16],
            0,
            0,
            0,
            true,
            false,
        );
        reader.seek(0).await.unwrap();

        let mut out = File::create(&out_path).await.unwrap();
        {
            let mut writer = BufWriter::new(&mut out);
            let mut hasher: ContentHasher = None;
            write_romfs_section(
                &mut reader,
                &mut writer,
                size,
                &counter,
                0,
                0,
                key_y,
                &mut hasher,
                &NoProgress,
                &CancelToken::new(),
            )
            .await
            .unwrap();
            writer.flush().await.unwrap();
        }

        let mut decrypted = Vec::new();
        File::open(&out_path)
            .await
            .unwrap()
            .read_to_end(&mut decrypted)
            .await
            .unwrap();

        assert_eq!(
            decrypted, plaintext,
            "pooled RomFS decrypt must match plaintext"
        );
    }

    /// When the cidx fixup is active (non-first CIA content, not single NCCH,
    /// not from NCSD) the legacy path XORed cidx into byte 1 of every chunk
    /// buffer, including chunks past the first and the partial tail. Spans more
    /// than two chunks so a regression that only fixes up chunk 0 is caught.
    #[tokio::test]
    async fn write_romfs_section_applies_cidx_fixup_to_every_chunk() {
        let key_y: u128 = 0x0123_4567_89AB_CDEF_0011_2233_4455_6677;
        let counter: [u8; 16] = [
            0xAA, 0xBB, 0xCC, 0xDD, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ];
        let cidx: u16 = 3;
        let size: u32 = (2 * CHUNK_SIZE + 0x1000) as u32;

        let plaintext: Vec<u8> = (0..size)
            .map(|i| (i.wrapping_mul(31) % 251) as u8)
            .collect();
        let key = derive_ctr_key(CTR_KEYS_0[0], key_y);

        // Build the encrypted input the decrypt path expects: per chunk, run the
        // continuous-keystream encrypt, then XOR cidx back into byte 1 so the
        // decrypt's per-chunk fixup cancels it out.
        let mut encrypted = plaintext.clone();
        Aes128Ctr::new_from_slices(&key, &counter)
            .unwrap()
            .apply_keystream(&mut encrypted);
        let mut offset = 0usize;
        while offset < encrypted.len() {
            encrypted[offset + 1] ^= cidx as u8;
            offset += CHUNK_SIZE;
        }

        let tmp = tempfile::tempdir().unwrap();
        let in_path = tmp.path().join("romfs.bin");
        std::fs::write(&in_path, &encrypted).unwrap();
        let out_path = tmp.path().join("out.bin");

        let in_file = File::open(&in_path).await.unwrap();
        let mut reader = CiaReader::new(
            in_file,
            false,
            in_path.clone(),
            [0u8; 16],
            0,
            cidx,
            0,
            false,
            false,
        );
        reader.seek(0).await.unwrap();

        let mut out = File::create(&out_path).await.unwrap();
        {
            let mut writer = BufWriter::new(&mut out);
            let mut hasher: ContentHasher = None;
            write_romfs_section(
                &mut reader,
                &mut writer,
                size,
                &counter,
                0,
                0,
                key_y,
                &mut hasher,
                &NoProgress,
                &CancelToken::new(),
            )
            .await
            .unwrap();
            writer.flush().await.unwrap();
        }

        let mut decrypted = Vec::new();
        File::open(&out_path)
            .await
            .unwrap()
            .read_to_end(&mut decrypted)
            .await
            .unwrap();

        assert_eq!(
            decrypted, plaintext,
            "per-chunk cidx fixup must hold across multiple chunks and the tail"
        );
    }

    /// Counterpart to the per-chunk fixup test: single-NCCH (and NCSD-sourced)
    /// content must NOT get the content-index fixup even when cidx is non-zero.
    /// This locks the `!(single_ncch || from_ncsd)` half of the gate so a
    /// regression that drops it and XORs byte 1 of every chunk is caught.
    #[tokio::test]
    async fn write_romfs_section_skips_cidx_fixup_for_single_ncch() {
        let key_y: u128 = 0x0123_4567_89AB_CDEF_0011_2233_4455_6677;
        let counter: [u8; 16] = [
            0xAA, 0xBB, 0xCC, 0xDD, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ];
        let cidx: u16 = 3;
        let size: u32 = 0x4000;

        let plaintext: Vec<u8> = (0..size)
            .map(|i| (i.wrapping_mul(31) % 251) as u8)
            .collect();
        let key = derive_ctr_key(CTR_KEYS_0[0], key_y);

        // Plain continuous-keystream encryption with no cidx XOR baked in: decrypt
        // must return the plaintext unchanged because single_ncch disables the
        // fixup even though cidx is non-zero.
        let mut encrypted = plaintext.clone();
        Aes128Ctr::new_from_slices(&key, &counter)
            .unwrap()
            .apply_keystream(&mut encrypted);

        let tmp = tempfile::tempdir().unwrap();
        let in_path = tmp.path().join("romfs.bin");
        std::fs::write(&in_path, &encrypted).unwrap();
        let out_path = tmp.path().join("out.bin");

        let in_file = File::open(&in_path).await.unwrap();
        let mut reader = CiaReader::new(
            in_file,
            false,
            in_path.clone(),
            [0u8; 16],
            0,
            cidx,
            0,
            true,
            false,
        );
        reader.seek(0).await.unwrap();

        let mut out = File::create(&out_path).await.unwrap();
        {
            let mut writer = BufWriter::new(&mut out);
            let mut hasher: ContentHasher = None;
            write_romfs_section(
                &mut reader,
                &mut writer,
                size,
                &counter,
                0,
                0,
                key_y,
                &mut hasher,
                &NoProgress,
                &CancelToken::new(),
            )
            .await
            .unwrap();
            writer.flush().await.unwrap();
        }

        let mut decrypted = Vec::new();
        File::open(&out_path)
            .await
            .unwrap()
            .read_to_end(&mut decrypted)
            .await
            .unwrap();

        assert_eq!(
            decrypted, plaintext,
            "cidx fixup must be skipped for single-NCCH content"
        );
    }

    #[test]
    fn extra_crypto_index_known_values() {
        assert_eq!(extra_crypto_index(0), 0);
        assert_eq!(extra_crypto_index(1), 1);
        assert_eq!(extra_crypto_index(10), 2);
        assert_eq!(extra_crypto_index(11), 3);
    }

    #[test]
    fn extra_crypto_index_unknown_defaults_to_zero() {
        assert_eq!(extra_crypto_index(2), 0);
        assert_eq!(extra_crypto_index(255), 0);
    }

    #[test]
    fn scramblekey_deterministic() {
        let key_x: u128 = 0x1234_5678_9ABC_DEF0_1234_5678_9ABC_DEF0;
        let key_y: u128 = 0xFEDC_BA98_7654_3210_FEDC_BA98_7654_3210;
        let result1 = scramblekey(key_x, key_y);
        let result2 = scramblekey(key_x, key_y);
        assert_eq!(result1, result2);
    }

    #[test]
    fn scramblekey_zero_inputs() {
        // ROL(ROL(0, 2) ^ 0 + C, 87) = ROL(C, 87)
        let result = scramblekey(0, 0);
        let expected = CTR_KEY_SCRAMBLE_C.rotate_left(87);
        assert_eq!(result, expected);
    }

    #[test]
    fn scramblekey_different_inputs_different_outputs() {
        let r1 = scramblekey(1, 0);
        let r2 = scramblekey(0, 1);
        assert_ne!(r1, r2);
    }

    #[test]
    fn derive_ctr_key_returns_16_bytes() {
        let key = derive_ctr_key(12345, 67890);
        assert_eq!(key.len(), 16);
    }
}
