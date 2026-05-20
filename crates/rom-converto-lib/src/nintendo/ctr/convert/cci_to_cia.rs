use crate::nintendo::ctr::constants::{
    CIA_CERT_CHAIN_SIZE, CIA_CONTENT_INDEX_SIZE, CTR_KEY_0X2C, CTR_KEYS_1, CTR_MEDIA_UNIT_SIZE,
    NCCH_FLAGS7_FIXED_KEY, NCCH_FLAGS7_NOCRYPTO,
};
use crate::nintendo::ctr::convert::template::{retail_cert_chain, template_ticket};
use crate::nintendo::ctr::decrypt::cia::{Aes128Ctr, derive_ctr_key, get_ncch_aes_counter};
use crate::nintendo::ctr::decrypt::model::NcchSection;
use crate::nintendo::ctr::models::cia::{CIA_HEADER_SIZE, CiaFileWithoutContent, CiaHeader};
use crate::nintendo::ctr::models::ncch_header::NcchHeader;
use crate::nintendo::ctr::models::ncsd_header::{NCSD_HEADER_SIZE, NcsdHeader};
use crate::nintendo::ctr::models::signature::{SignatureData, SignatureType};
use crate::nintendo::ctr::models::title_metadata::{
    ContentChunkRecord, ContentInfoRecord, ContentType, TitleMetadata, TitleMetadataHeader,
};
use crate::util::ProgressReporter;
use aes::cipher::{KeyIvInit, StreamCipher};
use anyhow::{Context, Result, bail};
use binrw::{BinRead, BinWrite, Endian};
use log::{debug, info};
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter, SeekFrom};

const NCCH_HEADER_SIZE: usize = 0x200;
const EXHEADER_REGION_SIZE: usize = 0x800;
const EXHEADER_FLAG_OFFSET: usize = 0xD;
const EXHEADER_SD_APPLICATION_BIT: u8 = 0x02;
const EXHEADER_SAVE_DATA_SIZE_OFFSET: usize = 0x1C0;
const NCCH_EXHEADER_HASH_OFFSET: usize = 0x160;
const CONTENT_COPY_BUF: usize = 4 * 1024 * 1024;

pub async fn cci_to_cia(
    input: &Path,
    output: &Path,
    progress: &dyn ProgressReporter,
) -> Result<()> {
    let mut in_file = File::open(input).await.context("opening CCI input")?;

    let mut ncsd_header_bytes = vec![0u8; NCSD_HEADER_SIZE];
    in_file
        .read_exact(&mut ncsd_header_bytes)
        .await
        .context("reading NCSD header")?;
    let ncsd =
        NcsdHeader::read(&mut Cursor::new(&ncsd_header_bytes)).context("parsing NCSD header")?;
    if ncsd.magic != NcsdHeader::MAGIC {
        bail!("input is not a valid NCSD file (missing NCSD magic at 0x100)");
    }

    let title_id = ncsd.media_id;
    debug!("CCI title ID: {title_id:016X}");

    let mut partitions: Vec<PartitionInfo> = Vec::new();
    for i in 0..3usize {
        let entry = &ncsd.partition_table[i];
        if entry.size == 0 {
            continue;
        }
        partitions.push(PartitionInfo {
            content_index: i as u16,
            offset: entry.offset as u64 * CTR_MEDIA_UNIT_SIZE as u64,
            size: entry.size as u64 * CTR_MEDIA_UNIT_SIZE as u64,
        });
    }

    if partitions.is_empty() || partitions[0].content_index != 0 {
        bail!("CCI has no executable partition 0; cannot convert to CIA");
    }

    let patched_cxi_prefix = patch_cxi_prefix(&mut in_file, &partitions[0]).await?;
    let save_data_size = patched_cxi_prefix.save_data_size;

    let total_size: u64 = partitions.iter().map(|p| p.size).sum();
    progress.start(total_size, "Converting CCI to CIA...");

    let mut chunk_records: Vec<ContentChunkRecord> = partitions
        .iter()
        .map(|p| ContentChunkRecord {
            content_id: p.content_index as u32,
            content_index: p.content_index,
            content_type: ContentType(0),
            content_size: p.size,
            hash: vec![0u8; 0x20],
        })
        .collect();

    for (i, p) in partitions.iter().enumerate() {
        let hash = hash_partition(&mut in_file, p, &patched_cxi_prefix).await?;
        chunk_records[i].hash = hash.to_vec();
    }

    let title_version = 0u16;

    let info_records = build_info_records(&chunk_records)?;
    let content_info_records_hash = hash_info_records(&info_records)?;

    let tmd = build_tmd(
        title_id,
        title_version,
        save_data_size,
        chunk_records.clone(),
        info_records,
        content_info_records_hash,
    );
    let ticket = build_ticket(title_id, title_version);

    let mut tmd_buf = Vec::new();
    tmd.write_options(&mut Cursor::new(&mut tmd_buf), Endian::Big, ())?;
    let tmd_size = tmd_buf.len() as u32;

    let mut tik_buf = Vec::new();
    ticket.write_options(&mut Cursor::new(&mut tik_buf), Endian::Big, ())?;
    let ticket_size = tik_buf.len() as u32;

    let mut cia_wo = CiaFileWithoutContent {
        header: CiaHeader {
            header_size: CIA_HEADER_SIZE,
            cia_type: 0,
            version: 0,
            cert_chain_size: CIA_CERT_CHAIN_SIZE,
            ticket_size,
            tmd_size,
            meta_size: 0,
            content_size: total_size,
            content_index: vec![0u8; CIA_CONTENT_INDEX_SIZE],
        },
        cert_chain: retail_cert_chain(),
        ticket,
        tmd,
    };
    for record in &cia_wo.tmd.content_chunk_records {
        cia_wo
            .header
            .set_content_index(record.content_index as usize);
    }

    let out = File::create(output).await.context("creating CIA output")?;
    let mut out = BufWriter::new(out);

    let mut preamble = Vec::new();
    cia_wo.write_options(&mut Cursor::new(&mut preamble), Endian::Little, ())?;
    out.write_all(&preamble).await?;

    let mut buf = vec![0u8; CONTENT_COPY_BUF];
    for p in &partitions {
        stream_partition(
            &mut in_file,
            &mut out,
            p,
            &patched_cxi_prefix,
            &mut buf,
            progress,
        )
        .await?;
    }

    out.flush().await?;
    progress.finish();

    info!(
        "Converted CCI to CIA: {} -> {}",
        input.display(),
        output.display()
    );
    Ok(())
}

#[derive(Debug, Clone)]
struct PartitionInfo {
    content_index: u16,
    offset: u64,
    size: u64,
}

struct PatchedCxiPrefix {
    ncch_header: [u8; NCCH_HEADER_SIZE],
    exheader: [u8; EXHEADER_REGION_SIZE],
    save_data_size: u32,
}

async fn patch_cxi_prefix(file: &mut File, p: &PartitionInfo) -> Result<PatchedCxiPrefix> {
    if p.size < (NCCH_HEADER_SIZE + EXHEADER_REGION_SIZE) as u64 {
        bail!("partition 0 too small to contain NCCH header + ExHeader");
    }
    file.seek(SeekFrom::Start(p.offset)).await?;
    let mut ncch_header_buf = vec![0u8; NCCH_HEADER_SIZE];
    file.read_exact(&mut ncch_header_buf).await?;
    let mut exheader_buf = vec![0u8; EXHEADER_REGION_SIZE];
    file.read_exact(&mut exheader_buf).await?;

    let ncch_header = NcchHeader::read_le(&mut Cursor::new(&ncch_header_buf))
        .context("parsing partition 0 NCCH header")?;

    let exhdr_hash_size = ncch_header.exhdrsize as usize;
    if exhdr_hash_size == 0 || exhdr_hash_size > EXHEADER_REGION_SIZE {
        bail!(
            "partition 0 NCCH header has invalid exhdrsize {:#x}",
            ncch_header.exhdrsize
        );
    }

    let encrypted = ncch_header.is_encrypted();
    let flags7 = ncch_header.flags[7];
    let uses_fixed_key = flags7 & NCCH_FLAGS7_FIXED_KEY != 0;
    let nocrypto = flags7 & NCCH_FLAGS7_NOCRYPTO != 0;

    let normal_key = if !encrypted || nocrypto {
        None
    } else if uses_fixed_key {
        let title_id_u64 = u64::from_le_bytes(ncch_header.titleid);
        let is_system = (title_id_u64 >> 32) & 0x10 != 0;
        let fixed = if is_system {
            CTR_KEYS_1[1]
        } else {
            CTR_KEYS_1[0]
        };
        Some(u128::to_be_bytes(fixed))
    } else {
        let key_y = u128::from_be_bytes(
            ncch_header.signature[..16]
                .try_into()
                .expect("16-byte slice"),
        );
        Some(derive_ctr_key(CTR_KEY_0X2C, key_y))
    };

    let counter = get_ncch_aes_counter(&ncch_header, NcchSection::ExHeader);

    if let Some(key) = normal_key {
        let mut cipher = Aes128Ctr::new(key.as_ref().into(), counter.as_ref().into());
        cipher.apply_keystream(&mut exheader_buf);
    }

    exheader_buf[EXHEADER_FLAG_OFFSET] |= EXHEADER_SD_APPLICATION_BIT;

    let mut hasher = Sha256::new();
    hasher.update(&exheader_buf[..exhdr_hash_size]);
    let new_hash = hasher.finalize();
    ncch_header_buf[NCCH_EXHEADER_HASH_OFFSET..NCCH_EXHEADER_HASH_OFFSET + 0x20]
        .copy_from_slice(&new_hash);

    let save_data_size = u32::from_le_bytes(
        exheader_buf[EXHEADER_SAVE_DATA_SIZE_OFFSET..EXHEADER_SAVE_DATA_SIZE_OFFSET + 4]
            .try_into()
            .expect("4-byte slice"),
    );

    if let Some(key) = normal_key {
        let mut cipher = Aes128Ctr::new(key.as_ref().into(), counter.as_ref().into());
        cipher.apply_keystream(&mut exheader_buf);
    }

    let mut header_arr = [0u8; NCCH_HEADER_SIZE];
    header_arr.copy_from_slice(&ncch_header_buf);
    let mut exhdr_arr = [0u8; EXHEADER_REGION_SIZE];
    exhdr_arr.copy_from_slice(&exheader_buf);

    Ok(PatchedCxiPrefix {
        ncch_header: header_arr,
        exheader: exhdr_arr,
        save_data_size,
    })
}

async fn hash_partition(
    file: &mut File,
    p: &PartitionInfo,
    patched: &PatchedCxiPrefix,
) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CONTENT_COPY_BUF];

    if p.content_index == 0 {
        hasher.update(patched.ncch_header);
        hasher.update(patched.exheader);
        let prefix_len = (NCCH_HEADER_SIZE + EXHEADER_REGION_SIZE) as u64;
        file.seek(SeekFrom::Start(p.offset + prefix_len)).await?;
        let mut remaining = p.size - prefix_len;
        while remaining > 0 {
            let to_read = remaining.min(buf.len() as u64) as usize;
            file.read_exact(&mut buf[..to_read]).await?;
            hasher.update(&buf[..to_read]);
            remaining -= to_read as u64;
        }
    } else {
        file.seek(SeekFrom::Start(p.offset)).await?;
        let mut remaining = p.size;
        while remaining > 0 {
            let to_read = remaining.min(buf.len() as u64) as usize;
            file.read_exact(&mut buf[..to_read]).await?;
            hasher.update(&buf[..to_read]);
            remaining -= to_read as u64;
        }
    }

    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok(out)
}

async fn stream_partition(
    file: &mut File,
    out: &mut BufWriter<File>,
    p: &PartitionInfo,
    patched: &PatchedCxiPrefix,
    buf: &mut [u8],
    progress: &dyn ProgressReporter,
) -> Result<()> {
    if p.content_index == 0 {
        out.write_all(&patched.ncch_header).await?;
        out.write_all(&patched.exheader).await?;
        progress.inc((NCCH_HEADER_SIZE + EXHEADER_REGION_SIZE) as u64);

        let prefix_len = (NCCH_HEADER_SIZE + EXHEADER_REGION_SIZE) as u64;
        file.seek(SeekFrom::Start(p.offset + prefix_len)).await?;
        let mut remaining = p.size - prefix_len;
        while remaining > 0 {
            let to_read = remaining.min(buf.len() as u64) as usize;
            file.read_exact(&mut buf[..to_read]).await?;
            out.write_all(&buf[..to_read]).await?;
            progress.inc(to_read as u64);
            remaining -= to_read as u64;
        }
    } else {
        file.seek(SeekFrom::Start(p.offset)).await?;
        let mut remaining = p.size;
        while remaining > 0 {
            let to_read = remaining.min(buf.len() as u64) as usize;
            file.read_exact(&mut buf[..to_read]).await?;
            out.write_all(&buf[..to_read]).await?;
            progress.inc(to_read as u64);
            remaining -= to_read as u64;
        }
    }
    Ok(())
}

fn build_tmd(
    title_id: u64,
    title_version: u16,
    save_data_size: u32,
    content_chunks: Vec<ContentChunkRecord>,
    content_info_records: Vec<ContentInfoRecord>,
    content_info_records_hash: Vec<u8>,
) -> TitleMetadata {
    let mut issuer = b"Root-CA00000003-CP0000000b".to_vec();
    issuer.resize(0x40, 0);

    TitleMetadata {
        signature_data: SignatureData {
            signature_type: SignatureType::Rsa2048Sha256,
            signature: vec![0u8; 0x100],
            padding: vec![0u8; 0x3C],
        },
        header: TitleMetadataHeader {
            signature_issuer: issuer,
            version: 1,
            ca_crl_version: 0,
            signer_crl_version: 0,
            reserved1: 0,
            system_version: 0,
            title_id,
            title_type: 0x00000040,
            group_id: 0,
            save_data_size,
            srl_private_save_data_size: 0,
            reserved2: 0,
            srl_flag: 0,
            reserved3: vec![0u8; 0x31],
            access_rights: 0,
            title_version,
            content_count: content_chunks.len() as u16,
            boot_content: 0,
            padding: 0,
            content_info_records_hash,
        },
        content_info_records,
        content_chunk_records: content_chunks,
    }
}

fn build_ticket(title_id: u64, title_version: u16) -> crate::nintendo::ctr::models::ticket::Ticket {
    let mut t = template_ticket();
    t.signature_data.signature = vec![0u8; 0x100];
    t.ticket_data.title_key = vec![0u8; 0x10];
    t.ticket_data.common_key_index = 0;
    t.ticket_data.title_id = title_id;
    t.ticket_data.ticket_title_version = title_version;
    t.ticket_data.console_id = 0;
    t.ticket_data.ticket_id = 0;
    t
}

fn build_info_records(chunks: &[ContentChunkRecord]) -> Result<Vec<ContentInfoRecord>> {
    let mut hasher = Sha256::new();
    for chunk in chunks {
        let mut buf = Vec::new();
        chunk.write_options(&mut Cursor::new(&mut buf), Endian::Big, ())?;
        hasher.update(&buf);
    }
    let first_hash = hasher.finalize().to_vec();

    let mut records = vec![
        ContentInfoRecord {
            content_index_offset: 0,
            content_command_count: 0,
            hash: vec![0u8; 0x20],
        };
        64
    ];
    records[0] = ContentInfoRecord {
        content_index_offset: 0,
        content_command_count: chunks.len() as u16,
        hash: first_hash,
    };
    Ok(records)
}

fn hash_info_records(records: &[ContentInfoRecord]) -> Result<Vec<u8>> {
    let mut hasher = Sha256::new();
    for r in records {
        let mut buf = Vec::new();
        r.write_options(&mut Cursor::new(&mut buf), Endian::Big, ())?;
        hasher.update(&buf);
    }
    Ok(hasher.finalize().to_vec())
}
