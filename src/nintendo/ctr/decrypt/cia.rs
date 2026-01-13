use aes::{
    Aes128,
    cipher::{KeyIvInit, StreamCipher},
};
use byteorder::{BigEndian, ByteOrder, LittleEndian};

use crate::nintendo::ctr::constants::{
    CTR_COMMON_KEYS_HEX, CTR_KEYS_0, CTR_KEYS_1, CTR_MEDIA_UNIT_SIZE, CTR_NCSD_PARTITIONS,
};
use crate::nintendo::ctr::decrypt::model::{CiaContent, NcchSection};
use crate::nintendo::ctr::decrypt::reader::CiaReader;
use crate::nintendo::ctr::decrypt::util::{cbc_decrypt, gen_iv};
use crate::nintendo::ctr::models::cia::CiaHeader;
use crate::nintendo::ctr::models::exe_fs_header::ExeFSHeader;
use crate::nintendo::ctr::models::ncch_header::NcchHeader;
use crate::nintendo::ctr::models::seeddb::SeedDatabase;
use crate::nintendo::ctr::util::align_64;
use anyhow::{Context, anyhow};
use binrw::BinRead;
use futures::future::select_ok;
use hex_literal::hex;
use lazy_static::lazy_static;
use log::debug;
use std::io::{Cursor, SeekFrom};
use std::{collections::HashMap, path::Path, vec};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter};

pub type Aes128Ctr = ctr::Ctr128BE<Aes128>;

const CHUNK_SIZE: usize = 32 * 1024 * 1024; // 32 MiB

fn extra_crypto_index(uses_extra_crypto: u8) -> usize {
    match uses_extra_crypto {
        0 => 0,
        1 => 1,
        10 => 2,
        11 => 3,
        _ => 0,
    }
}

fn derive_ctr_key(key_x: u128, key_y: u128) -> [u8; 16] {
    u128::to_be_bytes(scramblekey(key_x, key_y))
}

fn fixed_key(fixed_crypto: u8) -> Option<[u8; 16]> {
    (fixed_crypto != 0).then(|| u128::to_be_bytes(CTR_KEYS_1[(fixed_crypto as usize) - 1]))
}

async fn advance_to_offset(
    writer: &mut BufWriter<&mut File>,
    cia: &mut CiaReader,
    target_offset: u64,
) -> anyhow::Result<()> {
    if let Some(gap) = target_offset.checked_sub(writer.stream_position().await?)
        && gap > 0
    {
        let mut buf = vec![0u8; gap as usize];
        cia.read(&mut buf)
            .await
            .context("reading gap bytes before section")?;
        if writer.stream_position().await? == 512 {
            buf[1] = 0x00;
        }
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
) -> anyhow::Result<()> {
    let mut remaining_bytes = size;
    let mut buf = vec![0u8; CHUNK_SIZE];

    while remaining_bytes > CHUNK_SIZE as u32 {
        cia.read(&mut buf).await.context("reading plain chunk")?;
        writer
            .write_all(&buf)
            .await
            .context("writing plain chunk")?;
        remaining_bytes -= CHUNK_SIZE as u32;
    }

    if remaining_bytes > 0 {
        buf = vec![0u8; remaining_bytes as usize];
        cia.read(&mut buf)
            .await
            .context("reading final plain chunk")?;
        writer
            .write_all(&buf)
            .await
            .context("writing final plain chunk")?;
    }

    Ok(())
}

async fn write_exheader_section(
    cia: &mut CiaReader,
    writer: &mut BufWriter<&mut File>,
    size: u32,
    ctr: &[u8; 16],
    base_key: [u8; 16],
    fixed_crypto: u8,
) -> anyhow::Result<()> {
    let mut key = base_key;
    if let Some(fixed) = fixed_key(fixed_crypto) {
        key = fixed;
    }

    let mut buf = vec![0u8; size as usize];
    cia.read(&mut buf).await.context("reading ExHeader")?;
    Aes128Ctr::new_from_slices(&key, ctr)?.apply_keystream(&mut buf);
    writer.write_all(&buf).await.context("writing ExHeader")?;
    Ok(())
}

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

        for entry_idx in 0usize..10 {
            let entry_bytes = &decrypted_exefs[entry_idx * 16..(entry_idx + 1) * 16];
            let exe_info = ExeFSHeader::read(&mut Cursor::new(entry_bytes))?;

            let offset = LittleEndian::read_u32(&exe_info.file_offset) as usize + 512;
            let size = LittleEndian::read_u32(&exe_info.file_size) as usize;

            match exe_info.file_name.iter().rposition(|&x| x != 0) {
                Some(name_end) if exe_info.file_name[..=name_end].is_ascii() => {
                    let icon: [u8; 4] = hex!("69636f6e");
                    let banner: [u8; 6] = hex!("62616e6e6572");
                    if exe_info.file_name[..=name_end] != icon
                        && exe_info.file_name[..=name_end] != banner
                    {
                        decrypted_exefs.splice(
                            offset..offset + size,
                            extra_decrypted[offset..offset + size].iter().cloned(),
                        );
                    }
                }
                _ => {
                    decrypted_exefs.splice(
                        offset..offset + size,
                        extra_decrypted[offset..offset + size].iter().cloned(),
                    );
                }
            }
        }
    }

    writer
        .write_all(&decrypted_exefs)
        .await
        .context("writing ExeFS")?;
    Ok(())
}

async fn write_romfs_section(
    cia: &mut CiaReader,
    writer: &mut BufWriter<&mut File>,
    size: u32,
    ctr: &[u8; 16],
    uses_extra_crypto: u8,
    fixed_crypto: u8,
    key_y: u128,
) -> anyhow::Result<()> {
    let mut key = derive_ctr_key(CTR_KEYS_0[extra_crypto_index(uses_extra_crypto)], key_y);
    if let Some(fixed) = fixed_key(fixed_crypto) {
        key = fixed;
    }

    let mut remaining_bytes = size;
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut ctr_cipher = Aes128Ctr::new_from_slices(&key, ctr)?;

    while remaining_bytes > CHUNK_SIZE as u32 {
        cia.read(&mut buf).await.context("reading RomFS chunk")?;
        if cia.cidx > 0 && !(cia.single_ncch || cia.from_ncsd) {
            buf[1] ^= cia.cidx as u8;
        }
        ctr_cipher.apply_keystream(&mut buf);
        writer
            .write_all(&buf)
            .await
            .context("writing RomFS chunk")?;
        remaining_bytes -= CHUNK_SIZE as u32;
    }

    if remaining_bytes > 0 {
        buf = vec![0u8; remaining_bytes as usize];
        cia.read(&mut buf)
            .await
            .context("reading final RomFS chunk")?;
        if cia.cidx > 0 && !(cia.single_ncch || cia.from_ncsd) {
            buf[1] ^= cia.cidx as u8;
        }
        ctr_cipher.apply_keystream(&mut buf);
        writer
            .write_all(&buf)
            .await
            .context("writing final RomFS chunk")?;
    }

    Ok(())
}

fn get_ncch_aes_counter(hdr: &NcchHeader, section: NcchSection) -> [u8; 16] {
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
    rol(
        value.wrapping_add(42503689118608475533858958821215598218),
        87,
    )
}

async fn fetch_seed(title_id: &str) -> anyhow::Result<[u8; 16]> {
    lazy_static! {
        static ref CLIENT: reqwest::Client = reqwest::Client::builder()
            .tls_danger_accept_invalid_certs(true)
            .build()
            .expect("Failed to create HTTP client");
    }

    let countries = ["JP", "US", "GB", "KR", "TW", "AU", "NZ"];

    // Build a future for each country, returning Ok(bytes) on 200 or Err otherwise
    let requests = countries.iter().map(|&country| {
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

async fn write_to_file(
    ncch: &mut File,
    cia: &mut CiaReader,
    opts: NcchWriteOptions,
) -> anyhow::Result<()> {
    let mut buff_writer = BufWriter::new(ncch);

    advance_to_offset(&mut buff_writer, cia, opts.offset).await?;

    if !opts.encrypted {
        copy_plain_section(cia, &mut buff_writer, opts.size).await?;
        buff_writer.flush().await?;
        return Ok(());
    }

    let base_key = derive_ctr_key(CTR_KEYS_0[0], opts.keys[0]);

    match opts.section {
        NcchSection::ExHeader => {
            write_exheader_section(
                cia,
                &mut buff_writer,
                opts.size,
                &opts.counter,
                base_key,
                opts.fixed_crypto,
            )
            .await?
        }
        NcchSection::ExeFS => {
            write_exefs_section(
                cia,
                &mut buff_writer,
                ExefsDecryptOptions {
                    size: opts.size,
                    ctr: opts.counter,
                    base_key,
                    uses_extra_crypto: opts.uses_extra_crypto,
                    fixed_crypto: opts.fixed_crypto,
                    use_seed_crypto: opts.use_seed_crypto,
                    key_y: opts.keys[1],
                },
            )
            .await?
        }
        NcchSection::RomFS => {
            write_romfs_section(
                cia,
                &mut buff_writer,
                opts.size,
                &opts.counter,
                opts.uses_extra_crypto,
                opts.fixed_crypto,
                opts.keys[1],
            )
            .await?
        }
    };

    buff_writer.flush().await?;

    Ok(())
}

async fn get_new_key(key_y: u128, header: &NcchHeader, title_id: String) -> anyhow::Result<u128> {
    lazy_static! {
        static ref SEEDS: HashMap<String, [u8; 16]> = {
            let db_path = Path::new("seeddb.bin");
            if let Ok(data) = std::fs::read(db_path) {
                let seeddb =
                    SeedDatabase::read(&mut Cursor::new(data)).expect("failed to parse seeddb.bin");
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

    Ok(0)
}

pub async fn parse_ncch(
    cia: &mut CiaReader,
    offs: u64,
    mut title_id: [u8; 8],
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

    let uses_extra_crypto: u8 = header.flags[3];

    if uses_extra_crypto != 0 {
        debug!("  Uses extra NCCH crypto, keyslot 0x25");
    }

    let mut fixed_crypto: u8 = 0;
    let mut encrypted: bool = true;

    if (header.flags[7] & 1) != 0 {
        if (tid[3] & 16) != 0 {
            fixed_crypto = 2
        } else {
            fixed_crypto = 1
        }
        debug!("  Uses fixed-key crypto")
    }

    if (header.flags[7] & 4) != 0 {
        encrypted = false;
        debug!("  Not encrypted")
    }

    let use_seed_crypto: bool = (header.flags[7] & 32) != 0;
    let mut key_y = ncch_key_y;

    if use_seed_crypto {
        key_y = get_new_key(ncch_key_y, &header, hex::encode(title_id)).await?;
        debug!("Uses 9.6 NCCH Seed crypto with KeyY: {key_y:032X}");
    }

    let mut base: String;
    let file_name = cia.path.file_name().unwrap().to_string_lossy();

    if cia.single_ncch || cia.from_ncsd {
        base = file_name.strip_suffix(".3ds").unwrap().to_string();
    } else {
        base = file_name.strip_suffix(".cia").unwrap().to_string();
    }

    let absolute_path = cia.path.canonicalize()?;
    let final_path = if cfg!(windows) && absolute_path.to_string_lossy().starts_with(r"\\?\") {
        Path::new(&absolute_path.to_string_lossy()[4..].replace("\\", "/")).to_path_buf()
    } else {
        absolute_path
    };
    let parent_dir = final_path.parent().unwrap();

    base = format!(
        "{}/{}.{}.{:08X}.ncch",
        parent_dir.display(),
        base,
        if cia.from_ncsd {
            CTR_NCSD_PARTITIONS[cia.cidx as usize].to_string()
        } else {
            cia.cidx.to_string()
        },
        cia.content_id
    );

    let mut ncch: File = File::create(base.clone()).await?;
    tmp[399] = tmp[399] & 2 | 4;

    ncch.write_all(&tmp).await?;
    let mut counter: [u8; 16];
    if header.exhdrsize != 0 {
        counter = get_ncch_aes_counter(&header, NcchSection::ExHeader);
        write_to_file(
            &mut ncch,
            cia,
            NcchWriteOptions {
                offset: 512,
                size: header.exhdrsize * 2,
                section: NcchSection::ExHeader,
                counter,
                uses_extra_crypto,
                fixed_crypto,
                use_seed_crypto,
                encrypted,
                keys: [ncch_key_y, key_y],
            },
        )
        .await?;
    }

    if header.exefssize != 0 {
        counter = get_ncch_aes_counter(&header, NcchSection::ExeFS);
        write_to_file(
            &mut ncch,
            cia,
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
        )
        .await?;
    }

    if header.romfssize != 0 {
        counter = get_ncch_aes_counter(&header, NcchSection::RomFS);
        write_to_file(
            &mut ncch,
            cia,
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
        )
        .await?;
    }

    Ok(())
}

pub async fn parse_and_decrypt_cia(input: &Path, partition: Option<u8>) -> anyhow::Result<()> {
    debug!("Parsing CIA file: {}", input.display());

    let mut rom_file = File::open(input).await?;

    let mut data = Vec::new();
    rom_file.read_to_end(&mut data).await?;
    let mut cursor = Cursor::new(data);
    let cia_header = CiaHeader::read(&mut cursor)?;

    let cachainoff = align_64(cia_header.header_size as u64);
    let tikoff = align_64(cachainoff + cia_header.cert_chain_size as u64);
    let tmdoff = align_64(tikoff + cia_header.ticket_size as u64);
    let contentoffs = align_64(tmdoff + cia_header.tmd_size as u64);

    rom_file.seek(SeekFrom::Start(tikoff + 127 + 320)).await?;
    let mut enckey: [u8; 16] = [0; 16];
    rom_file.read_exact(&mut enckey).await?;
    rom_file.seek(SeekFrom::Start(tikoff + 156 + 320)).await?;
    let mut tid: [u8; 16] = [0; 16];
    rom_file.read_exact(&mut tid[0..8]).await?;

    if hex::encode(tid).starts_with("00048") {
        return Err(anyhow::anyhow!("Unsupported CIA file"));
    }

    rom_file.seek(SeekFrom::Start(tikoff + 177 + 320)).await?;
    let mut cmnkeyidx: u8 = 0;
    rom_file
        .read_exact(std::slice::from_mut(&mut cmnkeyidx))
        .await?;

    cbc_decrypt(&CTR_COMMON_KEYS_HEX[cmnkeyidx as usize], &tid, &mut enckey)?;
    let title_key = enckey;

    rom_file.seek(SeekFrom::Start(tmdoff + 518)).await?;
    let mut content_count: [u8; 2] = [0; 2];
    rom_file.read_exact(&mut content_count).await?;

    let mut next_content_offs = 0;
    for i in 0..BigEndian::read_u16(&content_count) {
        rom_file
            .seek(SeekFrom::Start(tmdoff + 2820 + (48 * i as u64)))
            .await?;
        // read the 16-byte content record once
        let mut cbuffer: [u8; 40] = [0; 40];
        rom_file.read_exact(&mut cbuffer).await?;

        let content = CiaContent {
            cid: BigEndian::read_u32(&cbuffer[0..4]),
            cidx: BigEndian::read_u16(&cbuffer[4..6]),
            ctype: BigEndian::read_u16(&cbuffer[6..8]),
            csize: BigEndian::read_u64(&cbuffer[8..16]),
        };

        let cenc = (content.ctype & 1) != 0;

        rom_file
            .seek(SeekFrom::Start(contentoffs + next_content_offs))
            .await?;
        let mut test: [u8; 512] = [0; 512];
        rom_file.read_exact(&mut test).await?;
        let mut search: [u8; 4] = test[256..260].try_into()?;

        let iv: [u8; 16] = gen_iv(content.cidx);

        if cenc {
            cbc_decrypt(&title_key, &iv, &mut test)?;
            search = test[256..260].try_into()?;
        }

        match std::str::from_utf8(&search) {
            Ok(utf8) => {
                if utf8 == "NCCH" {
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

                    if let Some(number) = partition
                        && (i as u8) != number
                    {
                        continue;
                    }
                    parse_ncch(&mut cia_handle, 0, tid[0..8].try_into()?).await?;
                } else {
                    return Err(anyhow!("Cia can't be parsed"));
                }
            }
            Err(e) => return Err(anyhow!(e)),
        }
    }

    Ok(())
}
